//! Issue 297: what a worker's and the curator's launch says about the person's configuration.
//!
//! The base's core runs with makiba's base folder as its `XDG_CONFIG_HOME`, and every terminal it
//! starts inherits that. The GitHub CLI reads an empty `GH_CONFIG_DIR` as unset (measured with gh
//! 2.96.0: it then reads `$XDG_CONFIG_HOME/gh/hosts.yml`), so the empty value a worker was given
//! held nothing. A worker and the curator now get `XDG_CONFIG_HOME` as `makibad` has it (removed
//! when it has none or an empty one), so git reads the person's own configuration, but only when
//! the settings prepared for the launch deny reading every credential file for that folder; and
//! `GH_CONFIG_DIR` is always a folder of the launch, `<state>/tmp/<launch id>/gh`.
//!
//! The daemon runs in this process on a stand-in base that keeps every launch spec. Each case
//! prepares and launches a document run (`t1`) and the curator. Two more runs wait at
//! `settings_ready` with a settings file the test writes: `t2` denies none of the credential
//! files (it stands for a launch prepared under another `XDG_CONFIG_HOME`), `t3` denies them all.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, EventStream, LaunchSpec, Terminal, TerminalRef,
};
use makiba::daemon::launch::add_launch;
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");

/// The stand-in, keeping every launch spec.
#[derive(Default)]
struct Recording {
    stub: Stub,
    launched: Mutex<Vec<LaunchSpec>>,
}

#[derive(Clone)]
struct Shared(Arc<Recording>);

impl Base for Shared {
    fn name(&self) -> &'static str {
        self.0.stub.name()
    }
    fn hello(&self) -> Result<BaseInfo, BaseError> {
        self.0.stub.hello()
    }
    fn launch(&self, spec: &LaunchSpec) -> Result<TerminalRef, BaseError> {
        self.0.launched.lock().unwrap().push(spec.clone());
        self.0.stub.launch(spec)
    }
    fn list(&self) -> Result<Vec<Terminal>, BaseError> {
        self.0.stub.list()
    }
    fn get(&self, t: &TerminalRef) -> Result<Terminal, BaseError> {
        self.0.stub.get(t)
    }
    fn read(&self, t: &TerminalRef, lines: usize) -> Result<Vec<String>, BaseError> {
        self.0.stub.read(t, lines)
    }
    fn keys(&self, t: &TerminalRef, keys: &[&str]) -> Result<(), BaseError> {
        self.0.stub.keys(t, keys)
    }
    fn send(&self, t: &TerminalRef, text: &str, only_if_idle: bool) -> Result<(), BaseError> {
        self.0.stub.send(t, text, only_if_idle)
    }
    fn end(&self, t: &TerminalRef) -> Result<(), BaseError> {
        self.0.stub.end(t)
    }
    fn events(&self) -> Result<EventStream, BaseError> {
        self.0.stub.events()
    }
    fn start(&self) -> Result<(), BaseError> {
        self.0.stub.start()
    }
    fn worker_denies(&self) -> BaseDenies {
        self.0.stub.worker_denies()
    }
}

/// The GitHub CLI's configuration folder, by its rule as measured: `GH_CONFIG_DIR` when not
/// empty, else `$XDG_CONFIG_HOME/gh` when not empty, else `$HOME/.config/gh`.
fn gh_dir(env: &BTreeMap<String, String>) -> PathBuf {
    let get = |k: &str| env.get(k).filter(|v| !v.is_empty());
    if let Some(d) = get("GH_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    if let Some(x) = get("XDG_CONFIG_HOME") {
        return Path::new(x).join("gh");
    }
    Path::new(get("HOME").expect("HOME")).join(".config/gh")
}

/// git's global configuration file under XDG, by its rule: `$XDG_CONFIG_HOME/git/config` when not
/// empty, else `$HOME/.config/git/config`.
fn git_xdg_config(env: &BTreeMap<String, String>) -> PathBuf {
    let get = |k: &str| env.get(k).filter(|v| !v.is_empty());
    match get("XDG_CONFIG_HOME") {
        Some(x) => Path::new(x).join("git/config"),
        None => Path::new(get("HOME").expect("HOME")).join(".config/git/config"),
    }
}

/// The credential files a worker's settings deny for `xdg` (`makibad`'s non-empty
/// `XDG_CONFIG_HOME`, or none): git's plain store at its default places, and under `xdg` git's
/// store and the GitHub CLI's login.
fn credential_files(xdg: Option<&Path>) -> Vec<String> {
    let mut files = vec![
        "~/.git-credentials".to_string(),
        "~/.config/git/credentials".to_string(),
    ];
    if let Some(x) = xdg {
        files.push(x.join("git/credentials").display().to_string());
        files.push(x.join("gh/hosts.yml").display().to_string());
    }
    files
}

/// A run waiting at `settings_ready` whose settings file denies reading `deny`.
fn waiting_run(w: &StateWriter, dir: &Path, root: &Path, task: &str, deny: &[String]) {
    w.upsert_run(&RunUpsert {
        task_id: task,
        project_slug: "p",
        status: Some("queued"),
        ..Default::default()
    })
    .unwrap();
    let work = root.join("docs").join(task);
    std::fs::create_dir_all(work.join("out")).unwrap();
    w.conn()
        .execute(
            "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
             VALUES ((SELECT id FROM runs WHERE task_id = ?1), 'spawn-' || ?1, 'document',
                     'fake', 'the brief', ?2, 'settings_ready')",
            [task, &work.to_string_lossy()],
        )
        .unwrap();
    let settings = dir.join("settings");
    std::fs::create_dir_all(&settings).unwrap();
    let file = serde_json::json!({
        "permissions": {"deny": deny.iter().map(|p| format!("Read({p})")).collect::<Vec<_>>()},
        "sandbox": {"enabled": true, "filesystem": {"denyRead": deny, "denyWrite": deny}},
    });
    std::fs::write(settings.join(format!("{task}.json")), file.to_string()).unwrap();
}

/// What one case launched: each launch's spec and its `run_launch.id`, by name.
struct Launched {
    state: PathBuf,
    specs: BTreeMap<String, (LaunchSpec, i64)>,
}

/// The daemon, with `xdg` and `gh` as its own `XDG_CONFIG_HOME` and `GH_CONFIG_DIR` (`None`:
/// unset), prepares and launches `t1` and the curator, and launches `t2` and `t3` (above).
fn launch_all(root: &Path, xdg: Option<&Path>, gh: Option<&Path>) -> Launched {
    let dir = root.join("state");
    std::fs::create_dir_all(&dir).unwrap();
    let _held_knowledge = makibad::use_knowledge(&root.join("knowledge"));
    let agent = serde_json::json!([AGENT, root.join("record"), "sleep:30000"]).to_string();
    // SAFETY: the only test of this binary, and no daemon of it runs while the environment changes.
    unsafe {
        std::env::set_var("MAKIBA_SHARE_DIR", root.join("share"));
        std::env::set_var("MAKIBA_PERSON_KEY", root.join("elsewhere/person.key"));
        std::env::set_var("MAKIBA_GOALS_DIR", root.join("goals"));
        std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
        std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
        std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
        std::env::set_var("MAKIBA_FAKE_AGENT", &agent);
        std::env::remove_var("MAKIBA_LAUNCH_AGENT");
        std::env::remove_var("MAKIBA_SECRETARY");
        match xdg {
            Some(x) => std::env::set_var("XDG_CONFIG_HOME", x),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        match gh {
            Some(g) => std::env::set_var("GH_CONFIG_DIR", g),
            None => std::env::remove_var("GH_CONFIG_DIR"),
        }
    }
    let effective = xdg.filter(|x| !x.as_os_str().is_empty());
    let curator = {
        let w = StateWriter::open(&dir.join("state.db")).unwrap();
        w.ensure_project("p", None, None).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: "p",
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree)
                 VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 'spawn-t1', 'document',
                         'fake', 'the brief', ?1)",
                [root.join("docs/t1").to_string_lossy()],
            )
            .unwrap();
        waiting_run(&w, &dir, root, "t2", &[]);
        waiting_run(&w, &dir, root, "t3", &credential_files(effective));
        let folder = root.join("cur");
        std::fs::create_dir_all(&folder).unwrap();
        add_launch(&w, Role::Curator, &folder, "b", None, "fake").unwrap()
    };
    let spawns = [
        ("t1", "spawn-t1".to_string()),
        ("t2", "spawn-t2".to_string()),
        ("t3", "spawn-t3".to_string()),
        ("curator", curator),
    ];

    let base = Shared(Arc::new(Recording::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let daemon = {
        let (dir, base, stop) = (dir.clone(), base.clone(), Arc::clone(&stop));
        std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(base.clone()), stop))
    };
    let until = Instant::now() + Duration::from_secs(20);
    let found = loop {
        let launched = base.0.launched.lock().unwrap().clone();
        let found: BTreeMap<String, LaunchSpec> = spawns
            .iter()
            .filter_map(|(name, spawn)| {
                launched
                    .iter()
                    .find(|s| s.tags.get("makiba.spawn") == Some(spawn))
                    .map(|s| (name.to_string(), s.clone()))
            })
            .collect();
        if found.len() == spawns.len() || Instant::now() > until {
            break found;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    stop.store(true, Ordering::SeqCst);
    let _ = daemon.join();

    let conn = Connection::open_with_flags(dir.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap();
    let mut specs = BTreeMap::new();
    for (name, spawn) in &spawns {
        let spec = found.get(*name).cloned().unwrap_or_else(|| {
            let why: Option<String> = conn
                .query_row(
                    "SELECT stage || ' ' || coalesce(failed_reason, '') FROM run_launch
                     WHERE spawn_id = ?1",
                    [spawn],
                    |r| r.get(0),
                )
                .ok();
            panic!("{name} was not launched within 20 s: {why:?}")
        });
        let id: i64 = conn
            .query_row(
                "SELECT id FROM run_launch WHERE spawn_id = ?1",
                [spawn],
                |r| r.get(0),
            )
            .unwrap();
        specs.insert(name.to_string(), (spec, id));
    }
    Launched { state: dir, specs }
}

/// A worker's and the curator's launch: `GH_CONFIG_DIR` is a folder of the launch, never empty;
/// `XDG_CONFIG_HOME` is the person's (as makibad has it) when the prepared settings deny every
/// credential file for it, and otherwise left as the core's terminal inherits it.
#[test]
fn a_worker_sees_the_persons_git_configuration_and_a_github_folder_of_its_own() {
    let home = std::env::var("HOME").expect("HOME");
    // (makibad's XDG_CONFIG_HOME, makibad's GH_CONFIG_DIR, a GH_CONFIG_DIR the core holds);
    // paths relative to the case's root.
    type Case = (Option<&'static str>, Option<&'static str>, &'static str);
    let cases: [Case; 4] = [
        (Some("person"), None, ""),
        (Some("person"), Some("own-gh"), "stale-gh"),
        (None, None, ""),
        (Some(""), Some(""), ""),
    ];
    for (xdg, gh, core_gh) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let at = |p: &str| {
            if p.is_empty() {
                PathBuf::new()
            } else {
                root.join(p)
            }
        };
        let person = xdg.map(at);
        let effective = person.clone().filter(|p| !p.as_os_str().is_empty());
        let own_gh = gh.map(at);
        // The base's folder, which the core's terminals inherit as their XDG_CONFIG_HOME.
        let base_folder = root.join("base-config/makiba/base");
        let launched = launch_all(&root, person.as_deref(), own_gh.as_deref());

        for (name, (spec, id)) in &launched.specs {
            let case = format!("{name}: XDG {xdg:?}, GH_CONFIG_DIR {gh:?}, core's {core_gh:?}");
            let t = launched.state.join("tmp").join(id.to_string()).join("gh");
            let t_s = t.display().to_string();

            // 1. GH_CONFIG_DIR is the launch's own folder: not empty, not makibad's own.
            assert_eq!(
                spec.env.get("GH_CONFIG_DIR"),
                Some(&t_s),
                "{case}: GH_CONFIG_DIR"
            );
            assert!(t.is_absolute(), "{case}: {t_s} is not absolute");
            if let Some(g) = own_gh.as_ref().filter(|g| !g.as_os_str().is_empty()) {
                assert_ne!(&t, g, "{case}: makibad's own GH_CONFIG_DIR");
            }
            assert!(
                !spec.unset.iter().any(|v| v == "GH_CONFIG_DIR"),
                "{case}: GH_CONFIG_DIR is unset: {:?}",
                spec.unset
            );

            // 2. XDG_CONFIG_HOME: the person's, removed when makibad has none; t2's settings
            //    deny none of the credential files, so it is left as inherited.
            let xdg_set = spec.env.get("XDG_CONFIG_HOME");
            let xdg_unset = spec.unset.iter().any(|v| v == "XDG_CONFIG_HOME");
            if name == "t2" {
                assert_eq!(xdg_set, None, "{case}: XDG_CONFIG_HOME given");
                assert!(!xdg_unset, "{case}: XDG_CONFIG_HOME removed");
            } else if let Some(x) = &effective {
                assert_eq!(
                    xdg_set,
                    Some(&x.display().to_string()),
                    "{case}: XDG_CONFIG_HOME"
                );
                assert!(!xdg_unset, "{case}: XDG_CONFIG_HOME also removed");
            } else {
                assert_eq!(xdg_set, None, "{case}: XDG_CONFIG_HOME given");
                assert!(xdg_unset, "{case}: XDG_CONFIG_HOME not removed");
            }

            // 3. In a terminal of the core (its environment, then the spec's env, then its
            //    unset), the GitHub CLI looks in the launch's folder only.
            let mut env = BTreeMap::from([
                ("HOME".to_string(), home.clone()),
                (
                    "XDG_CONFIG_HOME".to_string(),
                    base_folder.display().to_string(),
                ),
                (
                    "GH_CONFIG_DIR".to_string(),
                    at(core_gh).display().to_string(),
                ),
            ]);
            env.extend(spec.env.clone());
            for v in &spec.unset {
                env.remove(v);
            }
            let got = gh_dir(&env);
            assert_eq!(got, t, "{case}: the CLI's folder");
            assert!(!got.starts_with(&base_folder), "{case}: the base's folder");
            if let Some(x) = &effective {
                assert!(!got.starts_with(x), "{case}: the person's XDG folder");
            }
            assert!(
                !got.starts_with(Path::new(&home).join(".config/gh")),
                "{case}: the person's default folder"
            );

            // 4. git reads the person's XDG configuration file, never the base's; t2 keeps the
            //    base's, as before.
            let git = git_xdg_config(&env);
            if name == "t2" {
                assert_eq!(git, base_folder.join("git/config"), "{case}: git");
            } else {
                let want = match &effective {
                    Some(x) => x.join("git/config"),
                    None => Path::new(&home).join(".config/git/config"),
                };
                assert_eq!(git, want, "{case}: git's XDG configuration");
                assert!(!git.starts_with(&base_folder), "{case}: the base's folder");
            }
        }
    }
}
