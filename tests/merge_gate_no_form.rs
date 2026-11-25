//! Issue 433 (docs/design/merge-act.md section 8, build Issue 3): once makibad runs the landing
//! itself (the door verb `merge`), the secretary's merge gate hook allows no form. It refuses the
//! landing command in every form, the GraphQL enqueue mutation and any write to a
//! `pulls/<n>/merge*` path, naming the verb; it asks no door, so it answers the same with makibad
//! down. Reads and the dequeue mutation pass.
//!
//! Each case runs the real hook, `makiba hook merge-gate`, with a Bash payload on its input.

use serde_json::json;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The word, kept out of this file's own command lines.
const M: &str = "mer\x67e";
/// The verb every refusal names.
const V: &str = "mcp__makiba__merge";
const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn payload(cmd: &str) -> String {
    json!({"tool_name": "Bash", "tool_input": {"command": cmd}}).to_string()
}

fn hook(env: &[(&str, &str)]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_makiba"));
    c.args(["hook", "merge-gate"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        c.env(k, v);
    }
    c
}

/// The hook's exit code, standard output and standard error for `cmd`.
fn run(cmd: &str, env: &[(&str, &str)]) -> (i32, String, String) {
    let mut child = hook(env).spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload(cmd).as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// makibad down: no socket at the door's path and no key file.
fn down(dir: &Path) -> Vec<(String, String)> {
    vec![
        (
            "MAKIBA_DOOR".into(),
            dir.join("absent.sock").display().to_string(),
        ),
        (
            "MAKIBA_GATE_KEY".into(),
            dir.join("absent.key").display().to_string(),
        ),
    ]
}

fn pairs(env: &[(String, String)]) -> Vec<(&str, &str)> {
    env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

fn landing_forms() -> Vec<String> {
    let mut forms: Vec<String> = ["--squash", &format!("--{M}"), "--rebase"]
        .iter()
        .map(|m| format!("gh pr {M} 7 --repo o/r {m} --match-head-commit {SHA}"))
        .collect();
    forms.push(format!(
        "gh pr {M} 7 --repo o/r --squash --match-head-commit {SHA} --delete-branch"
    ));
    forms
}

fn refused_naming_the_verb(cmd: &str, env: &[(&str, &str)]) -> String {
    let (code, out, err) = run(cmd, env);
    assert_eq!(
        code, 2,
        "not refused: {cmd:?}\nstdout: {out}\nstderr: {err}"
    );
    assert!(
        err.contains(V),
        "the refusal of {cmd:?} does not name {V}: {err}"
    );
    assert!(
        !out.contains("permissionDecision"),
        "{cmd:?} was turned into a question: {out}"
    );
    err
}

#[test]
fn the_former_allowed_form_is_refused_naming_the_verb() {
    let tmp = tempfile::tempdir().unwrap();
    let env = down(tmp.path());
    for cmd in landing_forms() {
        let err = refused_naming_the_verb(&cmd, &pairs(&env));
        assert!(err.contains("L-K9"), "{err}");
        for old in ["cannot reach makibad", "gate's key", "merge with exactly"] {
            assert!(
                !err.contains(old),
                "the refusal of {cmd:?} still speaks of the door or the form ({old}): {err}"
            );
        }
    }
    // A key file from an older makibad changes nothing: the hook reads no key.
    std::fs::write(tmp.path().join("absent.key"), "k\n").unwrap();
    for cmd in landing_forms() {
        refused_naming_the_verb(&cmd, &pairs(&env));
    }
}

#[test]
fn the_enqueue_mutation_is_refused_naming_the_verb() {
    let tmp = tempfile::tempdir().unwrap();
    let env = down(tmp.path());
    for cmd in [
        format!(
            "gh api graphql -f query='mutation {{ enqueuePullRequest(input: {{pullRequestId: \"PR_1\", expectedHeadOid: \"{SHA}\"}}) {{ mergeQueueEntry {{ id }} }} }}'"
        ),
        "gh api graphql -f query=mutation{enqueuePullRequest(input:{pullRequestId:\"PR_1\"}){clientMutationId}}".to_string(),
    ] {
        let err = refused_naming_the_verb(&cmd, &pairs(&env));
        assert!(err.contains(&format!("{M} mutation")), "{err}");
    }
}

#[test]
fn a_write_to_any_pulls_merge_path_is_refused_naming_the_verb() {
    let tmp = tempfile::tempdir().unwrap();
    let env = down(tmp.path());
    for cmd in [
        format!("gh api -X PUT repos/o/r/pulls/7/{M}-async"),
        format!("gh api --method POST /repos/o/r/pulls/7/{M}-async"),
        format!("gh api -X PUT repos/o/r/pulls/7/{M}"),
        format!("gh api -X POST repos/o/r/pulls/7/{M}-queue"),
    ] {
        refused_naming_the_verb(&cmd, &pairs(&env));
    }
}

#[test]
fn reads_and_the_dequeue_pass_with_makibad_down() {
    let tmp = tempfile::tempdir().unwrap();
    let absent = down(tmp.path());
    let off = vec![
        ("MAKIBA_DOOR".to_string(), "none".to_string()),
        (
            "MAKIBA_GATE_KEY".to_string(),
            tmp.path().join("absent.key").display().to_string(),
        ),
    ];
    for cmd in [
        format!("gh pr view 7 --repo o/r --json state,{M}dAt"),
        format!("gh api repos/o/r/pulls/7/{M}"),
        format!("gh api repos/o/r/pulls/7/{M}-async"),
        "gh api graphql -f query='mutation { dequeuePullRequest(input: {id: \"MQE_1\"}) { clientMutationId } }'".to_string(),
        "git status".to_string(),
    ] {
        for env in [&absent, &off] {
            assert_eq!(
                run(&cmd, &pairs(env)),
                (0, String::new(), String::new()),
                "{cmd:?} with {env:?}"
            );
        }
    }
}

#[test]
fn the_hook_asks_no_door() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("door.sock");
    let key = tmp.path().join("gate.key");
    std::fs::write(&key, "k\n").unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&sock)
        .unwrap_or_else(|e| panic!("a Unix socket (this test needs a host that lets it): {e}"));
    listener.set_nonblocking(true).unwrap();
    let cmd = landing_forms().remove(0);
    let env = [
        ("MAKIBA_DOOR", sock.to_str().unwrap()),
        ("MAKIBA_GATE_KEY", key.to_str().unwrap()),
    ];
    let mut child = hook(&env).spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload(&cmd).as_bytes())
        .unwrap();
    // The hook must answer on its own; a connection to the door is a failure at once.
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok((_conn, _)) = listener.accept() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the hook connected to the door at {}", sock.display());
        }
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() > until {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the hook did not answer within 60 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().unwrap();
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    );
    assert_eq!(out.status.code(), Some(2), "{stdout}\n{stderr}");
    assert!(stderr.contains(V), "{stderr}");
    assert!(!stdout.contains("permissionDecision"), "{stdout}");
    assert_eq!(
        listener.accept().err().map(|e| e.kind()),
        Some(std::io::ErrorKind::WouldBlock),
        "nobody connects to the door"
    );
}
