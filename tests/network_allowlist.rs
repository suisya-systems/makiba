//! Issue 244: a strict network allow-list for workers and the curator, with the hosts a project's
//! builds need listed per project (line L-K2's network holder, and line L-K7 of
//! `docs/design/lines.md`).
//!
//! Everything runs in this process with no socket: the door is called with a person's or an
//! agent's key, and a document run (no clone) and a curator launch are prepared with the test
//! agent, whose settings file is the one Claude Code would be given.

use makiba::base::BaseDenies;
use makiba::base::stub::Stub;
use makiba::daemon::dispatch::Dispatch;
use makiba::daemon::door::{Door, issue_key};
use makiba::daemon::launch::add_launch;
use makiba::daemon::prepare::{Places, Prepare};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::{ErrorKind, Role};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

struct Org {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    door: Door,
    person: String,
}

impl Org {
    /// A state folder with projects `p` and `q` and the person's key.
    fn new() -> Org {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let w = StateWriter::open(&dir.join("state.db")).unwrap();
        w.ensure_project("p", None, Some("https://github.com/acme/p"))
            .unwrap();
        w.ensure_project("q", None, Some("https://github.com/acme/q"))
            .unwrap();
        let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
        let door = Door::new(w, Arc::new(Stub::new()), "fake".into());
        Org {
            _tmp: tmp,
            dir,
            door,
            person,
        }
    }

    fn writer(&self) -> StateWriter {
        StateWriter::open(&self.dir.join("state.db")).unwrap()
    }

    fn key(&self, role: Role, id: &str) -> String {
        issue_key(&self.writer(), role, id, None, None).unwrap()
    }

    fn call(&self, key: &str, verb: &str, args: Value) -> Result<Value, (ErrorKind, String)> {
        self.door
            .handle(Some(key), verb, args, None, false)
            .map_err(|e| (e.kind, format!("{} {}", e.message, e.hint)))
    }

    fn set(&self, key: &str, project: &str, value: &str) -> Result<Value, (ErrorKind, String)> {
        self.call(
            key,
            "config",
            json!({"action": "set", "key": "network_hosts", "project": project, "value": value}),
        )
    }

    /// The stored list of `project`, as the person's `config get` answers it.
    fn hosts(&self, project: &str) -> Value {
        let v = self
            .call(
                &self.person,
                "config",
                json!({"action": "get", "key": "network_hosts", "project": project}),
            )
            .unwrap_or_else(|e| panic!("config get network_hosts: {e:?}"));
        v["value"].clone()
    }

    fn places(&self) -> Places {
        Places {
            workers: self.dir.join("workers"),
            docs: self.dir.join("docs"),
            repos: self.dir.join("repos"),
            settings: self.dir.join("settings"),
            state_db: self.dir.join("state.db"),
        }
    }

    /// A worker of a document run of `project`, prepared: its settings file, parsed.
    fn worker_settings(&self, task: &str, project: &str) -> Value {
        let w = self.writer();
        w.upsert_run(&RunUpsert {
            task_id: task,
            project_slug: project,
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree)
                 VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?1 || '-s', 'document',
                         'fake', 'b', ?2)",
                [task, &self.dir.join("docs").join(task).to_string_lossy()],
            )
            .unwrap();
        Dispatch::new(4).pass(&w).unwrap();
        let ready = Prepare::new(self.places(), BaseDenies::default())
            .pass(&w)
            .unwrap();
        assert!(ready.contains(&task.to_string()), "not prepared: {ready:?}");
        read(&self.dir.join("settings").join(format!("{task}.json")))
    }

    /// The curator, prepared: its settings file, parsed.
    fn curator_settings(&self) -> Value {
        let w = self.writer();
        let folder = self.dir.join("curator");
        let spawn = add_launch(&w, Role::Curator, &folder, "b", None, "fake").unwrap();
        Prepare::new(self.places(), BaseDenies::default())
            .pass(&w)
            .unwrap();
        read(
            &self
                .dir
                .join("settings")
                .join(format!("curator-{spawn}.json")),
        )
    }
}

fn read(path: &Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("no settings file {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap()
}

/// The settings refuse every host but `hosts` without asking, and no permission rule widens the
/// list (Claude Code adds the domain of a `WebFetch(domain:...)` allow rule to it).
fn assert_strict_with(v: &Value, hosts: &[&str]) {
    let network = &v["sandbox"]["network"];
    assert_eq!(
        network["strictAllowlist"],
        json!(true),
        "not strict: {network}"
    );
    assert_eq!(network["allowedDomains"], json!(hosts), "{network}");
    let allow = v["permissions"]["allow"].as_array().cloned();
    for rule in allow.unwrap_or_default() {
        let rule = rule.as_str().unwrap_or_default();
        assert!(!rule.starts_with("WebFetch"), "a rule widens it: {rule}");
    }
}

/// 1. A worker of a project with one listed host: strict, and exactly that host. `status` shows
///    the list per project, and `none` for a project with no host listed.
#[test]
fn a_workers_settings_hold_the_strict_list_and_exactly_the_projects_host() {
    let org = Org::new();
    org.set(&org.person, "p", "registry.npmjs.org")
        .unwrap_or_else(|e| panic!("the person could not list a host: {e:?}"));
    assert_eq!(org.hosts("p"), json!("registry.npmjs.org"));
    assert_strict_with(&org.worker_settings("t1", "p"), &["registry.npmjs.org"]);

    let v = makiba::status_local::read(&org.dir.join("state.db")).unwrap();
    let entry = |slug: &str| {
        v["projects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["slug"] == json!(slug))
            .cloned()
            .unwrap_or_else(|| panic!("no {slug}: {v}"))
    };
    assert_eq!(
        entry("p")["network_hosts"],
        json!("registry.npmjs.org"),
        "{v}"
    );
    assert_eq!(entry("q")["network_hosts"], json!(""), "{v}");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_makiba"))
        .arg("status")
        .env("MAKIBA_STATE_DIR", &org.dir)
        .env("MAKIBA_DOOR", "none")
        .env("MAKIBA_BASE", "stub")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let line = |slug: &str| {
        text.lines()
            .find(|l| l.trim_start().starts_with(&format!("{slug}:")))
            .unwrap_or_else(|| panic!("no line of {slug}: {text}"))
            .to_string()
    };
    assert!(
        line("p").contains("network hosts: registry.npmjs.org"),
        "{text}"
    );
    assert!(line("q").contains("network hosts: none"), "{text}");
}

/// 2. With no host listed (a project registered before this change has none) the list is empty
///    and strict; so is the curator's, which has no project.
#[test]
fn with_no_host_listed_the_list_is_empty_and_strict() {
    let org = Org::new();
    assert_strict_with(&org.worker_settings("t2", "q"), &[]);
    assert_strict_with(&org.curator_settings(), &[]);
}

/// 3. GitHub's hosts are refused, by `config set` and at registration, with the reason; so is an
///    entry that is not a host name. The stored list is unchanged.
#[test]
fn a_github_host_is_refused_at_registration_and_after() {
    let org = Org::new();
    org.set(&org.person, "p", "registry.npmjs.org").unwrap();
    for bad in [
        "github.com",
        "api.github.com",
        "GitHub.com",
        "*.github.com",
        "registry.npmjs.org,uploads.github.com",
    ] {
        let (kind, why) = org.set(&org.person, "p", bad).unwrap_err();
        assert_eq!(kind, ErrorKind::Usage, "{bad}: {why}");
        assert!(why.contains("github.com"), "{bad}: no reason: {why}");
    }
    for bad in [
        "https://registry.npmjs.org",
        "registry.npmjs.org/x",
        "registry.npmjs.org:443",
        "*",
        "*.org",
        "140.82.112.3",
    ] {
        assert!(org.set(&org.person, "p", bad).is_err(), "{bad} was taken");
    }
    assert_eq!(org.hosts("p"), json!("registry.npmjs.org"));

    let refused = org.call(
        &org.person,
        "project",
        json!({"slug": "r", "repo": "acme/r", "hosts": "github.com"}),
    );
    let (kind, why) = refused.expect_err("project took github.com as a host");
    assert_eq!(kind, ErrorKind::Usage, "{why}");
    assert!(why.contains("github.com"), "{why}");
    assert_eq!(org.hosts("r"), Value::Null);

    let taken = org.call(
        &org.person,
        "project",
        json!({"slug": "s", "repo": "acme/s", "hosts": "static.crates.io"}),
    );
    assert!(taken.is_ok(), "{taken:?}");
    assert_eq!(org.hosts("s"), json!("static.crates.io"));
}

/// 4. An agent's key cannot change the list: `config` is the person's, and the secretary's
///    `project` naming hosts is denied; a `project` without hosts (a fetch again) keeps the list.
#[test]
fn an_agent_key_cannot_change_the_list() {
    let org = Org::new();
    org.call(
        &org.person,
        "config",
        json!({"action": "set", "key": "allowed_repo_owners", "value": "acme"}),
    )
    .unwrap();
    org.set(&org.person, "p", "registry.npmjs.org").unwrap();
    let secretary = org.key(Role::Secretary, "secretary");
    let worker = org.key(Role::Worker, "w-t");
    let curator = org.key(Role::Curator, "curator");
    for key in [&secretary, &worker, &curator] {
        let (kind, why) = org.set(key, "p", "example.org").unwrap_err();
        assert_eq!(kind, ErrorKind::Denied, "{why}");
    }
    let (kind, why) = org
        .call(
            &secretary,
            "project",
            json!({"slug": "p", "repo": "acme/p", "hosts": "example.org"}),
        )
        .expect_err("the secretary listed a host");
    assert_eq!(kind, ErrorKind::Denied, "{why}");
    assert_eq!(org.hosts("p"), json!("registry.npmjs.org"));
    org.call(
        &secretary,
        "project",
        json!({"slug": "p", "repo": "acme/p"}),
    )
    .unwrap();
    assert_eq!(org.hosts("p"), json!("registry.npmjs.org"));
}

/// 5. Lines L-K2 and L-K7 name this holder and no longer the Issue.
#[test]
fn lines_k2_and_k7_name_this_holder() {
    let doc =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/design/lines.md"))
            .unwrap();
    for id in ["L-K2", "L-K7"] {
        let row = doc
            .lines()
            .find(|l| l.starts_with(&format!("| {id} ")))
            .unwrap_or_else(|| panic!("lines.md has no {id} row"));
        assert!(
            !row.contains("`Open: #244`"),
            "{id} still names #244: {row}"
        );
        assert!(
            row.contains("`tests/network_allowlist.rs::"),
            "{id} does not name tests/network_allowlist.rs: {row}"
        );
    }
}
