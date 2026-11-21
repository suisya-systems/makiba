//! A worker's review by a second reader (Issue 135, line L-K10), carried from claude-org-ja's
//! `tools/codex_review_gate.sh` and `tools/codex_round_cap.py`. The door's verb `review` counts
//! the rounds (`door.rs`); this module runs one round: it decides which reader can run, gives it
//! a snapshot of the run's committed head, and reads its answer.
//!
//! Readers, chosen per call: the other model's program (`codex` by default) when it is on
//! makibad's PATH and its `login status` exits 0; otherwise, or when it fails before it read
//! anything, the same model in a fresh session (`claude -p`, a process of its own that has seen
//! nothing of the worker's session: no resume, no MCP server and so no door key, only the reading
//! tools, sandboxed settings with no network host). The answer names the reader.
//!
//! The reader never runs in the worker's folder: makibad writes the run's committed head (read
//! with the hardened git of `run_repo`, never a value the worker wrote) and the diff from the
//! project clone's base into a folder of its own under `<state>/tmp`, and removes it after.
//!
//! An answer is `passed` (established) only when the reader exited 0, read the repository at
//! least once (a successful command or tool result in its event stream) and ended with the line
//! `FINDINGS: blocker=<n> major=<n> minor=<n>`. An answer carrying a token shape of the secret
//! scanner or the content of the person or gate key is withheld.

use super::prepare::Places;
use super::run_repo;
use crate::agents::{self, Role, TaskSettings};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The design review's rounds per task, fixed (carried).
pub const DESIGN_ALLOWED: i64 = 1;
/// The diff review's rounds per task until the secretary or the person changes it (carried).
pub const DIFF_ALLOWED: i64 = 3;
/// A reader still running after this is stopped; the round is not established. The tool call's
/// own timeout (`agents::mcp_config`) is above it.
pub const BOUND: Duration = Duration::from_secs(20 * 60);
/// The name of the fallback reader in an answer.
pub const FRESH: &str = "fresh-session";
/// The files the reader finds beside the tree, in its folder.
pub const DIFF_FILE: &str = "MAKIBA_REVIEW.diff";
pub const NOTE_FILE: &str = "MAKIBA_DESIGN.md";

const FORM: &str = "List each finding on its own line as `[Blocker] <file>:<line> - <finding> - <fix>` (or `[Major]`, `[Minor]`). Blocker: wrong or unsafe, must be fixed. Major: should be fixed before it lands. Minor: may be left. Do not change any file. End with exactly one line `FINDINGS: blocker=<n> major=<n> minor=<n>`.";

/// The two programs, read when makibad starts (`MAKIBA_REVIEW_OTHER`, default `codex`;
/// `MAKIBA_REVIEW_SAME`, default `claude`) so that no request can choose them.
#[derive(Clone, Debug)]
pub struct Programs {
    pub other: PathBuf,
    pub same: PathBuf,
}

impl Programs {
    pub fn from_env() -> Programs {
        let var = |k: &str, d: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(d))
        };
        Programs {
            other: var("MAKIBA_REVIEW_OTHER", "codex"),
            same: var("MAKIBA_REVIEW_SAME", "claude"),
        }
    }
}

/// The program `p` as it would run: a path as it is, a bare name looked up on PATH.
fn resolve(p: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    if p.components().count() > 1 {
        return std::fs::metadata(p)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .then(|| p.to_path_buf());
    }
    agents::on_path(&p.to_string_lossy())
}

fn name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Which readers can start: the other model's program (or why not) and the same model's.
pub struct Plan {
    pub other: Result<PathBuf, String>,
    pub same: Option<PathBuf>,
}

impl Plan {
    pub fn find(p: &Programs) -> Plan {
        let other = match resolve(&p.other) {
            None => Err(format!("{} not on PATH", name(&p.other))),
            Some(path) => {
                let mut c = Command::new(&path);
                c.args(["login", "status"]);
                match bounded(c, None, Duration::from_secs(30)) {
                    Ok(r) if r.ok => Ok(path),
                    Ok(r) => Err(format!(
                        "{} login status: {}",
                        name(&p.other),
                        first_line(&r.err).unwrap_or("not logged in")
                    )),
                    Err(e) => Err(format!("{} login status: {e}", name(&p.other))),
                }
            }
        };
        Plan {
            other,
            same: resolve(&p.same),
        }
    }

    pub fn none(&self) -> Option<String> {
        match (&self.other, &self.same) {
            (Err(why), None) => Some(format!("{why}; the same model's program not on PATH")),
            _ => None,
        }
    }
}

/// The folder of one round: the tree at the head, the diff, the note, the fallback's settings.
pub struct Snapshot {
    pub dir: PathBuf,
    pub tree: PathBuf,
    pub base: String,
    pub head: String,
    top: PathBuf,
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        let _ = run_repo::remove_under(&self.top, &self.dir);
    }
}

fn plain(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
        && !s.contains("..")
}

/// Write the snapshot of the run `task`: the base is the project clone's `origin/<branch>` (or
/// its local branch), the head the run's `HEAD`, both commit ids read by makibad.
pub fn snapshot(
    places: &Places,
    task: &str,
    project: &str,
    branch: &str,
    workplace: &Path,
    note: Option<&str>,
) -> Result<Snapshot, String> {
    if !plain(branch) {
        return Err(format!(
            "the project's default branch {branch:?} is not a plain name"
        ));
    }
    let gitdir = run_repo::dir(&places.settings, task);
    if !run_repo::is_run(&gitdir, workplace) {
        return Err(format!(
            "the run has no repository of its own at {}",
            gitdir.display()
        ));
    }
    let clone = places.repos.join(project);
    let at_clone = |r: &str| {
        let mut c = Command::new("git");
        c.arg("-C")
            .arg(&clone)
            .args(["-c", run_repo::HOOKS_OFF])
            .args(["rev-parse", "--verify", "-q", &format!("{r}^{{commit}}")])
            .stdin(Stdio::null());
        run_repo::out(c, "git rev-parse")
            .ok()
            .filter(|s| !s.is_empty())
    };
    let base = at_clone(&format!("refs/remotes/origin/{branch}"))
        .or_else(|| at_clone(&format!("refs/heads/{branch}")))
        .ok_or_else(|| format!("the project clone {} has no {branch}", clone.display()))?;
    let head = run_repo::commit(&gitdir, workplace, "HEAD")
        .ok_or("the run's repository has no commit at HEAD")?;
    let since = run_repo::read(&gitdir, workplace, &["merge-base", &base, &head])?;
    let top = super::prepare::tmp_root(&places.settings);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let dir = top.join(format!("review-{task}-{stamp}"));
    let tree = dir.join("tree");
    std::fs::create_dir_all(&tree).map_err(|e| format!("{}: {e}", tree.display()))?;
    let snap = Snapshot {
        dir: std::path::absolute(&dir).unwrap_or(dir),
        tree: std::path::absolute(&tree).unwrap_or(tree),
        base,
        head,
        top,
    };
    // The tree at the head, without the folders an agent program reads its settings from.
    let mut archive = run_repo::git(&gitdir, workplace);
    archive
        .args(["archive", "--format=tar", &snap.head])
        .stdout(Stdio::piped());
    let mut a = archive.spawn().map_err(|e| format!("git archive: {e}"))?;
    let tar = Command::new("tar")
        .arg("-x")
        .arg("-C")
        .arg(&snap.tree)
        .stdin(a.stdout.take().ok_or("git archive: no output")?)
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("tar: {e}"))?;
    let archived = a.wait().map_err(|e| format!("git archive: {e}"))?;
    if !archived.success() || !tar.status.success() {
        return Err(format!(
            "the head could not be written for the reader: {}",
            String::from_utf8_lossy(&tar.stderr).trim()
        ));
    }
    for d in [".claude", ".codex"] {
        let p = snap.tree.join(d);
        if p.symlink_metadata().is_ok() {
            run_repo::remove_under(&snap.tree, &p)?;
        }
    }
    let mut diff = run_repo::git(&gitdir, workplace);
    diff.args([
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        &since,
        &snap.head,
    ]);
    let d = diff.output().map_err(|e| format!("git diff: {e}"))?;
    if !d.status.success() {
        return Err(format!(
            "git diff: {}",
            String::from_utf8_lossy(&d.stderr).trim()
        ));
    }
    let write = |name: &str, bytes: &[u8]| {
        std::fs::write(snap.tree.join(name), bytes).map_err(|e| format!("{name}: {e}"))
    };
    write(DIFF_FILE, &d.stdout)?;
    if let Some(n) = note {
        write(NOTE_FILE, n.as_bytes())?;
    }
    Ok(snap)
}

/// What a finished child left.
struct Ran {
    ok: bool,
    out: String,
    err: String,
    timed_out: bool,
}

/// Run `c` with `input` on standard input, stopped after `bound`.
fn bounded(mut c: Command, input: Option<&str>, bound: Duration) -> Result<Ran, String> {
    c.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| e.to_string())?;
    if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
        let text = text.to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        });
    }
    let pipe = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = Vec::new();
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut s);
            }
            String::from_utf8_lossy(&s).into_owned()
        })
    };
    let out = pipe(
        child
            .stdout
            .take()
            .map(|o| Box::new(o) as Box<dyn Read + Send>),
    );
    let err = pipe(
        child
            .stderr
            .take()
            .map(|o| Box::new(o) as Box<dyn Read + Send>),
    );
    let until = Instant::now() + bound;
    let (status, timed_out) = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(s) => break (Some(s), false),
            None if Instant::now() >= until => {
                let _ = child.kill();
                let _ = child.wait();
                break (None, true);
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    Ok(Ran {
        ok: status.is_some_and(|s| s.success()),
        out: out.join().unwrap_or_default(),
        err: err.join().unwrap_or_default(),
        timed_out,
    })
}

fn first_line(s: &str) -> Option<&str> {
    s.lines().map(str::trim).find(|l| !l.is_empty())
}

/// The answer of one round.
#[derive(Debug)]
pub struct Answer {
    pub established: bool,
    pub reader: String,
    pub fallback_reason: Option<String>,
    pub reason: Option<String>,
    pub findings: Option<[i64; 3]>,
    pub text: String,
}

/// What a reader's event stream showed.
#[derive(Default)]
struct Stream {
    reads: usize,
    text: String,
    error: Option<String>,
}

/// `codex exec --json`: completed command executions and the last agent message.
fn read_other(out: &str) -> Stream {
    let mut r = Stream::default();
    for v in out
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
    {
        let item = &v["item"];
        match (v["type"].as_str(), item["type"].as_str()) {
            (Some("item.completed"), Some("command_execution"))
                if item["exit_code"] == json!(0) =>
            {
                r.reads += 1
            }
            (Some("item.completed"), Some("agent_message")) => {
                r.text = item["text"].as_str().unwrap_or_default().to_string()
            }
            (Some("turn.failed"), _) => r.error = v["error"]["message"].as_str().map(String::from),
            (Some("error"), _) => r.error = v["message"].as_str().map(String::from),
            _ => {}
        }
    }
    r
}

/// `claude -p --output-format stream-json`: tool results that are not errors, and the result.
fn read_same(out: &str) -> Stream {
    let mut r = Stream::default();
    for v in out
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
    {
        match v["type"].as_str() {
            Some("user") => {
                r.reads += v["message"]["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|c| c["type"] == "tool_result" && c["is_error"] != json!(true))
                    .count()
            }
            Some("result") => {
                r.text = v["result"].as_str().unwrap_or_default().to_string();
                if v["is_error"] == json!(true) {
                    r.error = Some(format!(
                        "the session ended in error: {}",
                        v["subtype"].as_str().unwrap_or("?")
                    ));
                }
            }
            _ => {}
        }
    }
    r
}

/// The last `FINDINGS:` line of `text`.
pub fn findings(text: &str) -> Option<[i64; 3]> {
    let re = regex::Regex::new(r"FINDINGS: blocker=(\d+) major=(\d+) minor=(\d+)").ok()?;
    let c = re.captures_iter(text).last()?;
    let n = |i: usize| c[i].parse::<i64>().ok();
    Some([n(1)?, n(2)?, n(3)?])
}

fn prompt(design: bool) -> String {
    if design {
        format!(
            "The file {NOTE_FILE} in this folder is a design for a change to the repository whose tree is this folder. Verify it against the repository: check that every file, function, flag, path and behaviour it relies on exists and works as the design assumes, and that the design reaches its stated goal without gaps. {FORM}"
        )
    } else {
        format!(
            "This folder is a repository's tree at the head of a change; the file {DIFF_FILE} here is the change (git diff from its base to its head). Read {DIFF_FILE} and the files it touches, and review the change for bugs, missing cases, broken contracts and missing tests. {FORM}"
        )
    }
}

/// The settings of the fresh session: the worker role's, a write surface of its own, no network
/// host, the keys, `state.db` and the person's credentials denied.
fn same_settings(places: &Places, snap: &Snapshot) -> Result<PathBuf, String> {
    let scratch = snap.dir.join("scratch");
    std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
    let abs = |ps: Vec<PathBuf>| -> Vec<PathBuf> {
        ps.into_iter()
            .map(|p| {
                if p.starts_with("~") {
                    p
                } else {
                    std::path::absolute(&p).unwrap_or(p)
                }
            })
            .collect()
    };
    let (cred_dirs, cred_files) = super::prepare::credentials();
    let db = places.state_db.display().to_string();
    let task = TaskSettings {
        write_dirs: vec![scratch],
        deny_dirs: abs(cred_dirs),
        deny_files: abs([
            crate::cli::person_key_path(),
            crate::cli::gate_key_path(),
            PathBuf::from(&db),
            PathBuf::from(format!("{db}-wal")),
            PathBuf::from(format!("{db}-shm")),
        ]
        .into_iter()
        .chain(cred_files)
        .collect()),
        deny_write_dirs: vec![crate::cli::share_dir()],
        deny_write_files: vec![],
        role: Some(Role::Worker),
        allowed_hosts: Some(vec![]),
    };
    let file = snap.dir.join("settings.json");
    agents::by_name("claude_code")
        .ok_or("no claude_code agent module")?
        .write_task_settings(&file, &task)
        .map_err(|e| e.to_string())?;
    Ok(file)
}

fn run_other(program: &Path, snap: &Snapshot, design: bool) -> Result<(Ran, Stream), String> {
    let mut c = Command::new(program);
    c.current_dir(&snap.tree).args([
        "exec",
        "--json",
        "--skip-git-repo-check",
        "-c",
        "sandbox_mode=\"read-only\"",
        "-c",
        "approval_policy=\"never\"",
        "-c",
        "mcp_servers={}",
        &prompt(design),
    ]);
    let ran = bounded(c, None, BOUND)?;
    let read = read_other(&ran.out);
    Ok((ran, read))
}

fn run_same(
    program: &Path,
    places: &Places,
    snap: &Snapshot,
    design: bool,
) -> Result<(Ran, Stream), String> {
    let settings = same_settings(places, snap)?;
    let mut c = Command::new(program);
    c.current_dir(&snap.tree)
        .env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1")
        .args([
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--no-session-persistence",
            "--setting-sources",
            "project,local",
            "--strict-mcp-config",
            "--mcp-config",
            "{\"mcpServers\":{}}",
            "--permission-mode",
            "dontAsk",
            "--settings",
        ])
        .arg(&settings)
        .args(["--tools", "Read,Grep,Glob"]);
    let ran = bounded(c, Some(&prompt(design)), BOUND)?;
    let read = read_same(&ran.out);
    Ok((ran, read))
}

/// Judge one reader's run.
fn judge(reader: String, ran: &Ran, read: Stream) -> Answer {
    let found = findings(&read.text);
    let reason = if ran.timed_out {
        Some(format!("stopped after {} minutes", BOUND.as_secs() / 60))
    } else if !ran.ok {
        Some(
            read.error
                .clone()
                .or_else(|| first_line(&ran.err).map(String::from))
                .unwrap_or_else(|| "the reader exited with a failure".into()),
        )
    } else if let Some(e) = &read.error {
        Some(e.clone())
    } else if read.reads == 0 {
        Some("the reader read nothing of the repository (an empty pass)".into())
    } else if found.is_none() {
        Some("the answer has no FINDINGS line".into())
    } else {
        None
    };
    Answer {
        established: reason.is_none(),
        reader,
        fallback_reason: None,
        reason,
        findings: found,
        text: read.text,
    }
}

/// Run one round with the readers of `plan` on `snap`.
pub fn run(plan: &Plan, places: &Places, snap: &Snapshot, design: bool) -> Answer {
    let fallback_reason = match &plan.other {
        Ok(program) => match run_other(program, snap, design) {
            Ok((ran, read)) => {
                // A failure before anything was read is the program not usable: fall back once.
                if ran.ok || read.reads > 0 || plan.same.is_none() {
                    return withheld(judge(name(program), &ran, read));
                }
                let why = read
                    .error
                    .clone()
                    .or_else(|| first_line(&ran.err).map(String::from))
                    .unwrap_or_else(|| "it exited with a failure before reading".into());
                format!("{} failed: {why}", name(program))
            }
            Err(e) => format!("{} could not start: {e}", name(program)),
        },
        Err(why) => why.clone(),
    };
    let Some(same) = &plan.same else {
        return Answer {
            established: false,
            reader: "none".into(),
            fallback_reason: None,
            reason: Some(fallback_reason),
            findings: None,
            text: String::new(),
        };
    };
    let mut a = match run_same(same, places, snap, design) {
        Ok((ran, read)) => judge(FRESH.into(), &ran, read),
        Err(e) => Answer {
            established: false,
            reader: FRESH.into(),
            fallback_reason: None,
            reason: Some(format!("the fresh session could not start: {e}")),
            findings: None,
            text: String::new(),
        },
    };
    a.fallback_reason = Some(fallback_reason);
    withheld(a)
}

/// An answer with a token shape or a key's content in it comes back empty, not established.
fn withheld(mut a: Answer) -> Answer {
    let keys: Vec<String> = [crate::cli::person_key_path(), crate::cli::gate_key_path()]
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .map(|k| k.trim().to_string())
        .filter(|k| k.len() >= 16)
        .collect();
    let hit = crate::hooks::git_hook::find(a.text.as_bytes())
        .map(|(shape, _)| shape.to_string())
        .or_else(|| {
            keys.iter()
                .any(|k| a.text.contains(k.as_str()))
                .then(|| "a makiba key".to_string())
        });
    if let Some(shape) = hit {
        a.established = false;
        a.reason = Some(format!("the answer was withheld: it carried {shape}"));
        a.findings = None;
        a.text = String::new();
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_findings_line_counts() {
        assert_eq!(
            findings("FINDINGS: blocker=9 major=9 minor=9\nx\nFINDINGS: blocker=1 major=2 minor=3"),
            Some([1, 2, 3])
        );
        assert_eq!(findings("FINDINGS: none"), None);
    }
}
