//! Issue 198: the acceptance gate fetches through the project clone's own `origin`.
//!
//! - With a clone whose `origin` is an SSH-style URL of the pull request's repository, the gate
//!   fetches through `origin` and reaches a verdict.
//! - With an `origin` that names another repository, the gate refuses and says why; nothing is
//!   fetched from it.
//!
//! No network: the clone's `origin` URL is mapped to a local repository by `url.<path>.insteadOf`
//! in the clone's own git configuration, and `https://github.com/` is mapped to a missing folder,
//! so a fetch from the pull request's browser URL fails locally. The global and system git
//! configuration are not read. The gate runs in the door (`Door::handle` with the merge gate's
//! key); `gh` is a stand-in that answers `pr view` and nothing else.

mod common;

use makiba::base::stub::Stub;
use makiba::daemon::door::Door;
use makiba::daemon::gate;
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Once, OnceLock};

const GATE_KEY: &str = "198198198198198198198198198198198198198198198198198198198198ab12";

/// `pr view <n> --repo o/<name> ...` prints `<name>/view.<n>`; any other call fails.
const STAND_IN: &str = r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  name=${5#*/}; cat "$FAKE_GH_DIR/$name/view.$3"; exit 0
fi
echo "unexpected call: $*" >&2; exit 2
"#;

static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
static INIT: Once = Once::new();

fn init() -> PathBuf {
    INIT.call_once(|| {
        let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        let script = home.path().join("gh");
        common::exe::write(&script, STAND_IN);
        std::fs::create_dir_all(home.path().join("repos")).unwrap();
        // SAFETY: set once, before this file's tests read the environment.
        unsafe {
            std::env::set_var("MAKIBA_GH", &script);
            std::env::set_var("FAKE_GH_DIR", home.path());
            std::env::set_var("MAKIBA_REPOS_DIR", home.path().join("repos"));
            std::env::set_var(gate::MODE_ENV, "human");
            std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
            std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        }
    });
    HOME.get().unwrap().path().to_path_buf()
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A served repository with `main` and PR 7, the project's clone of it whose `origin` is
/// `origin_url` (mapped to the served repository), `state.db` and a door.
struct Case {
    _tmp: tempfile::TempDir,
    name: String,
    served: PathBuf,
    clone: PathBuf,
    door: Door,
    head: String,
}

impl Case {
    fn new(name: &str, origin_url: &str) -> Case {
        let home = init();
        let tmp = tempfile::tempdir().unwrap();
        let served = tmp.path().join("served");
        std::fs::create_dir_all(&served).unwrap();
        git_in(&served, &["init", "-q", "-b", "main"]);
        std::fs::write(served.join("a.txt"), "a\n").unwrap();
        git_in(&served, &["add", "a.txt"]);
        git_in(&served, &["commit", "-q", "-m", "base"]);

        let repos = home.join("repos");
        let clone = repos.join(name);
        assert!(!clone.exists(), "the case name {name} is used twice");
        git_in(&repos, &["clone", "-q", served.to_str().unwrap(), name]);
        git_in(&clone, &["remote", "set-url", "origin", origin_url]);
        let rewrite = format!("url.{}.insteadOf", served.display());
        git_in(&clone, &["config", &rewrite, origin_url]);
        let nowhere = format!("url.{}/nowhere/.insteadOf", tmp.path().display());
        git_in(&clone, &["config", &nowhere, "https://github.com/"]);

        // PR 7: one commit on a branch; then main moves on after the clone was made.
        git_in(&served, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(served.join("b.txt"), "b\n").unwrap();
        git_in(&served, &["add", "b.txt"]);
        git_in(&served, &["commit", "-q", "-m", "change"]);
        let head = git_in(&served, &["rev-parse", "HEAD"]);
        git_in(&served, &["update-ref", "refs/pull/7/head", &head]);
        git_in(&served, &["checkout", "-q", "main"]);
        std::fs::write(served.join("c.txt"), "c\n").unwrap();
        git_in(&served, &["add", "c.txt"]);
        git_in(&served, &["commit", "-q", "-m", "main moves"]);

        let gh = home.join(name);
        std::fs::create_dir_all(&gh).unwrap();
        let view = json!({"number": 7, "url": format!("https://github.com/o/{name}/pull/7"),
                          "baseRefName": "main", "headRefOid": head});
        std::fs::write(gh.join("view.7"), view.to_string()).unwrap();

        let w = StateWriter::open(&tmp.path().join("state.db")).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: name,
            title: Some("T"),
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "INSERT OR IGNORE INTO projects (slug, display_name) VALUES (?1, ?1)",
                [name],
            )
            .unwrap();
        w.conn()
            .execute(
                "UPDATE projects SET origin_url = ?2 WHERE slug = ?1",
                [name, &format!("https://github.com/o/{name}")],
            )
            .unwrap();
        w.append_event(&NewEvent {
            kind: "ci_completed",
            actor: Some("makibad"),
            payload: Some(
                &json!({"repo": format!("o/{name}"), "pr": 7, "head_oid": head,
                        "status": "passed", "fail_count": 0, "pending_count": 0,
                        "total_checks": 1, "probe_attempts": 1}),
            ),
            ..Default::default()
        })
        .unwrap();
        let door = Door::new(w, Arc::new(Stub::new()), "claude_code".into());
        door.set_gate_key(GATE_KEY);
        Case {
            _tmp: tmp,
            name: name.into(),
            served,
            clone,
            door,
            head,
        }
    }

    fn gate(&self) -> Value {
        self.door
            .handle(
                Some(GATE_KEY),
                "gate",
                json!({"repo": format!("o/{}", self.name), "pr": 7, "head": self.head}),
                None,
                false,
            )
            .unwrap_or_else(|e| panic!("gate: {e:?}"))
    }

    /// The gate's refs in the clone: `<ref> <id>` lines.
    fn gate_refs(&self) -> String {
        git_in(
            &self.clone,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/makiba/gate/",
            ],
        )
    }
}

fn reaches_a_verdict(name: &str, origin_url: &str) {
    let c = Case::new(name, origin_url);
    let tracking = git_in(&c.clone, &["rev-parse", "refs/remotes/origin/main"]);
    let v = c.gate();
    assert_eq!(v["verdict"], json!("no_manifest"), "{origin_url}: {v}");
    assert_eq!(v["allow"], json!(true), "{origin_url}: {v}");
    let r = format!("refs/makiba/gate/o/{name}/7");
    let refs = c.gate_refs();
    assert!(
        refs.contains(&format!("{r}/head {}", c.head)),
        "{origin_url}: head not fetched through origin: {refs}"
    );
    let main = git_in(&c.served, &["rev-parse", "main"]);
    assert!(
        refs.contains(&format!("{r}/base {main}")),
        "{origin_url}: base not fetched through origin: {refs}"
    );
    assert_eq!(
        git_in(&c.clone, &["rev-parse", "refs/remotes/origin/main"]),
        tracking,
        "{origin_url}: the gate moved origin's tracking ref"
    );
}

#[test]
fn scp_style_origin_of_the_repository_reaches_a_verdict() {
    reaches_a_verdict("scp", "git@github.com:o/scp.git");
}

#[test]
fn ssh_url_origin_in_other_case_without_dot_git_reaches_a_verdict() {
    reaches_a_verdict("sshurl", "ssh://git@github.com/O/SshUrl");
}

#[test]
fn origin_naming_another_repository_is_refused_and_nothing_fetched() {
    let other = "git@github.com:o/someone-else.git";
    let c = Case::new("other", other);
    // The served repository publishes PR 7: a fetch through this origin would succeed.
    let v = c.gate();
    assert_eq!(v["verdict"], json!("rejected"), "{v}");
    assert_eq!(v["allow"], json!(false), "{v}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains(other), "the reason names the origin: {v}");
    assert!(
        reason.contains("not o/other"),
        "the reason says it is not the PR's repository: {v}"
    );
    assert_eq!(c.gate_refs(), "", "nothing is fetched from that origin");
}

#[test]
fn origin_with_two_urls_is_refused_and_nothing_fetched() {
    let c = Case::new("twourls", "git@github.com:o/twourls.git");
    git_in(
        &c.clone,
        &[
            "config",
            "--add",
            "remote.origin.url",
            "git@github.com:o/elsewhere.git",
        ],
    );
    let v = c.gate();
    assert_eq!(v["verdict"], json!("rejected"), "{v}");
    assert!(
        v["reason"].as_str().unwrap().contains("single origin"),
        "{v}"
    );
    assert_eq!(c.gate_refs(), "", "nothing is fetched");
}
