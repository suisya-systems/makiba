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
//! Both readers run inside an OS-level boundary that makibad sets up outside the reader's own
//! program (Issue 262): bubblewrap (`MAKIBA_REVIEW_BOUNDARY`, default `bwrap`) with a mount plan
//! of its own (`bounded_reader`). The reader sees the system folders, its program's own folders,
//! the round's tree (read-only), its scratch and settings, and a home of the round holding, of
//! the person's home, only its login (and codex's configuration, read-only); the keys, the state
//! folder, the credentials, the base's folders and both readers' configuration are covered where
//! a folder given holds them. A home that lies in a system folder is hidden, and the reader's own
//! files installed under it are given back after it (Issue 568). When the boundary cannot start,
//! no reader runs (fail closed).
//!
//! An answer is `passed` (established) only when the reader exited 0, opened at least one file
//! of the change under review (the diff file or a file it changes; for a design the note), judged
//! from its recorded commands or tool calls (a recognised read command with the file among its
//! words that exited 0; a `Read` or file `Grep` whose result is not an error), and ended with the line
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

/// The two programs and the boundary they run in, read when makibad starts
/// (`MAKIBA_REVIEW_OTHER`, default `codex`; `MAKIBA_REVIEW_SAME`, default `claude`;
/// `MAKIBA_REVIEW_BOUNDARY`, default `bwrap`) so that no request can choose them.
#[derive(Clone, Debug)]
pub struct Programs {
    pub other: PathBuf,
    pub same: PathBuf,
    pub boundary: PathBuf,
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
            boundary: var("MAKIBA_REVIEW_BOUNDARY", "bwrap"),
        }
    }
}

/// The program `p` as it would run: a path made absolute against makibad's own folder (a reader
/// starts in the round's tree, where a relative path would name the worker's files), a bare name
/// looked up on PATH.
fn resolve(p: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    if p.components().count() > 1 {
        let p = std::path::absolute(p).ok()?;
        return std::fs::metadata(&p)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .then_some(p);
    }
    agents::on_path(&p.to_string_lossy())
}

fn name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Which readers can start: the boundary they run in (or why it cannot start), the other
/// model's program (or why not) and the same model's.
pub struct Plan {
    pub boundary: Result<PathBuf, String>,
    pub other: Result<PathBuf, String>,
    pub same: Option<PathBuf>,
}

/// Can the boundary make its namespaces here? It is asked to run `true` in one.
fn boundary_starts(p: &Path) -> Result<PathBuf, String> {
    let path = resolve(p).ok_or_else(|| format!("{} not on PATH", name(p)))?;
    let mut c = Command::new(&path);
    c.args([
        "--ro-bind",
        "/",
        "/",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--unshare-pid",
        "--die-with-parent",
        "--",
        "true",
    ]);
    match bounded(c, None, Duration::from_secs(30)) {
        Ok(r) if r.ok => Ok(path),
        Ok(r) => Err(first_line(&r.err)
            .unwrap_or("it exited with a failure")
            .to_string()),
        Err(e) => Err(e),
    }
}

impl Plan {
    pub fn find(p: &Programs) -> Plan {
        let boundary = boundary_starts(&p.boundary).map_err(|why| {
            format!(
                "the reader's boundary {} cannot start: {why}",
                name(&p.boundary)
            )
        });
        if let Err(why) = &boundary {
            // No reader runs outside the boundary (Issue 262): nothing else is asked.
            return Plan {
                boundary: Err(why.clone()),
                other: Err(why.clone()),
                same: None,
            };
        }
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
            boundary,
            other,
            same: resolve(&p.same),
        }
    }

    pub fn none(&self) -> Option<String> {
        if let Err(why) = &self.boundary {
            return Some(why.clone());
        }
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
    /// The files the diff changes (paths in the tree, as `git diff --name-only` gives them).
    changed: Vec<String>,
    top: PathBuf,
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        let _ = run_repo::remove_under(&self.top, &self.dir);
    }
}

/// Remove `p` in the snapshot's tree: a link or a file itself (never what it names), a folder
/// with what is under it.
fn remove_in(tree: &Path, p: &Path) -> Result<(), String> {
    match p.symlink_metadata() {
        Err(_) => Ok(()),
        Ok(m) if m.is_dir() => run_repo::remove_under(tree, p),
        Ok(_) => std::fs::remove_file(p).map_err(|e| format!("{}: {e}", p.display())),
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
    let mut snap = Snapshot {
        dir: std::path::absolute(&dir).unwrap_or(dir),
        tree: std::path::absolute(&tree).unwrap_or(tree),
        base,
        head,
        changed: vec![],
        top,
    };
    // The tree at the head, without the folders an agent program reads its settings from.
    write_tree(&gitdir, workplace, &snap.head, &snap.tree)?;
    for d in [".claude", ".codex"] {
        remove_in(&snap.tree, &snap.tree.join(d))?;
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
    let names = run_repo::git(&gitdir, workplace)
        .args([
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            &since,
            &snap.head,
        ])
        .output()
        .map_err(|e| format!("git diff --name-only: {e}"))?;
    if !names.status.success() {
        return Err(format!(
            "git diff --name-only: {}",
            String::from_utf8_lossy(&names.stderr).trim()
        ));
    }
    snap.changed = names
        .stdout
        .split(|b| *b == 0)
        .filter(|n| !n.is_empty())
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .collect();
    // The tree is the worker's: a path of these names there (a link out of the folder, say) is
    // removed, and the file is made new, never written through what was there.
    let write = |name: &str, bytes: &[u8]| -> Result<(), String> {
        let p = snap.tree.join(name);
        remove_in(&snap.tree, &p)?;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&p)
            .and_then(|mut f| f.write_all(bytes))
            .map_err(|e| format!("{name}: {e}"))
    };
    write(DIFF_FILE, &d.stdout)?;
    if let Some(n) = note {
        write(NOTE_FILE, n.as_bytes())?;
    }
    Ok(snap)
}

/// Write the commit `head` of the run into `tree` as it is recorded: every blob byte for byte
/// (no export or filter attribute applies, as it would in `git archive`), a link as a plain file
/// holding its target (so nothing in the folder leads out of it), submodules left out.
fn write_tree(gitdir: &Path, workplace: &Path, head: &str, tree: &Path) -> Result<(), String> {
    let list = run_repo::git(gitdir, workplace)
        .args(["ls-tree", "-r", "-z", "--full-tree", head])
        .output()
        .map_err(|e| format!("git ls-tree: {e}"))?;
    if !list.status.success() {
        return Err(format!(
            "git ls-tree: {}",
            String::from_utf8_lossy(&list.stderr).trim()
        ));
    }
    // `<mode> blob <oid>\t<path>`, NUL-separated.
    let mut files: Vec<(bool, String, PathBuf)> = vec![];
    for e in list.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let e = String::from_utf8_lossy(e);
        let Some((meta, path)) = e.split_once('\t') else {
            continue;
        };
        let m: Vec<&str> = meta.split(' ').collect();
        if m.len() != 3 || m[1] != "blob" {
            continue;
        }
        let rel = Path::new(path);
        if rel.is_absolute()
            || rel
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(format!(
                "the head records a path that is not plain: {path:?}"
            ));
        }
        files.push((m[0] == "100755", m[2].to_string(), tree.join(rel)));
    }
    let mut cat = run_repo::git(gitdir, workplace);
    cat.args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child = cat.spawn().map_err(|e| format!("git cat-file: {e}"))?;
    let mut input = child.stdin.take().ok_or("git cat-file: no input")?;
    let oids: String = files.iter().map(|(_, o, _)| format!("{o}\n")).collect();
    let feeder = std::thread::spawn(move || {
        let _ = input.write_all(oids.as_bytes());
    });
    let mut out = std::io::BufReader::new(child.stdout.take().ok_or("git cat-file: no output")?);
    let written = (|| -> Result<(), String> {
        use std::io::BufRead;
        for (exec, oid, dest) in &files {
            let mut header = String::new();
            out.read_line(&mut header).map_err(|e| e.to_string())?;
            let size: usize = header
                .split_whitespace()
                .nth(2)
                .and_then(|n| n.parse().ok())
                .ok_or_else(|| format!("git cat-file: no blob {oid}: {}", header.trim()))?;
            let mut body = vec![0u8; size + 1];
            out.read_exact(&mut body).map_err(|e| e.to_string())?;
            body.pop();
            if let Some(dir) = dest.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dest)
                .and_then(|mut f| f.write_all(&body))
                .map_err(|e| format!("{}: {e}", dest.display()))?;
            if *exec {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755));
            }
        }
        Ok(())
    })();
    drop(out);
    let _ = child.kill();
    let _ = child.wait();
    let _ = feeder.join();
    written.map_err(|e| format!("the head could not be written for the reader: {e}"))
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
    // Its own process group, so that a stop at the bound ends what it started too.
    std::os::unix::process::CommandExt::process_group(&mut c, 0);
    let mut child = c.spawn().map_err(|e| e.to_string())?;
    if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
        let text = text.to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        });
    }
    // Each pipe is read on a thread that hands its text over a channel: a descendant still holding
    // the pipe cannot keep the round waiting past the bound.
    let pipe = |r: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut s = Vec::new();
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut s);
            }
            let _ = tx.send(String::from_utf8_lossy(&s).into_owned());
        });
        rx
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
                // SAFETY: a signal to the process group this function made for the child.
                unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                let _ = child.wait();
                break (None, true);
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    // The group is ended either way: nothing the reader started outlives the round.
    // SAFETY: as above; the group is gone or holds only the reader's descendants.
    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
    let rest = |rx: std::sync::mpsc::Receiver<String>| {
        rx.recv_timeout(until.saturating_duration_since(Instant::now()) + Duration::from_secs(5))
            .unwrap_or_default()
    };
    Ok(Ran {
        ok: status.is_some_and(|s| s.success()),
        out: rest(out),
        err: rest(err),
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
    /// Successful actions that opened a file under review.
    reads: usize,
    /// Every action seen (a command or a tool call), for the reason of a round without a read.
    seen: Vec<String>,
    text: String,
    error: Option<String>,
}

/// The files whose opening proves a read: the canonical tree and the canonical files in it that
/// the reader is meant to read (the diff and the files it changes; the design note for a design).
struct Targets {
    tree: PathBuf,
    files: Vec<PathBuf>,
}

impl Targets {
    fn of(snap: &Snapshot, design: bool) -> Targets {
        let tree = std::fs::canonicalize(&snap.tree).unwrap_or_else(|_| snap.tree.clone());
        let names: Vec<&str> = if design {
            vec![NOTE_FILE]
        } else {
            std::iter::once(DIFF_FILE)
                .chain(snap.changed.iter().map(String::as_str))
                .collect()
        };
        let files = names
            .into_iter()
            .filter_map(|n| std::fs::canonicalize(tree.join(n)).ok())
            .filter(|p| p.is_file() && p.starts_with(&tree))
            .collect();
        Targets { tree, files }
    }

    /// Does `path` (as the reader wrote it, relative to the tree) name one of the files? It is
    /// resolved as the filesystem does, so `..` and a link cannot lead out of the tree.
    fn names(&self, path: &str) -> bool {
        std::fs::canonicalize(self.tree.join(path))
            .map(|c| c.starts_with(&self.tree) && self.files.contains(&c))
            .unwrap_or(false)
    }
}

/// Programs that only look and never end or steer the shell: a command string with any other
/// program (`exit`, `false`, `if`, `eval`, a script) is not trusted to have run what it says.
const LOOKERS: [&str; 7] = ["pwd", "ls", "echo", "wc", "find", "tree", "sort"];

/// The programs whose operand files are read, and nothing else.
const READERS: [&str; 10] = [
    "cat", "head", "tail", "sed", "nl", "less", "more", "grep", "rg", "bat",
];

/// The simple commands of a shell string, each as its words, or `None` when the string is more
/// than a sequence of plain commands (`;` or `|` between them): no `&&`, `||`, `&`, comment,
/// redirection, substitution, expansion, escape or subshell, so that what ran is what the words say. The shell
/// of the reader's wrapper (`bash -lc '...'`) is looked through.
fn simple_commands(command: &str) -> Option<Vec<Vec<String>>> {
    let words = |s: &str| -> Option<Vec<Vec<String>>> {
        let mut out: Vec<Vec<String>> = vec![];
        let (mut cur, mut word, mut has) = (vec![], String::new(), false);
        let mut quote = None;
        let mut it = s.chars().peekable();
        while let Some(c) = it.next() {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some('\''), c) => word.push(c),
                (Some(_), '$' | '`' | '\\') => return None,
                (Some(_), c) => word.push(c),
                (None, '\'' | '"') => (quote, has) = (Some(c), true),
                // `2>/dev/null` and `2>&1` as words of their own (outside quotes) are no operand.
                (None, '>') if has && word == "2" => {
                    let rest: String = it.clone().collect();
                    let n = ["/dev/null", "&1"]
                        .iter()
                        .find(|r| {
                            rest.strip_prefix(**r).is_some_and(|a| {
                                a.chars()
                                    .next()
                                    .is_none_or(|c| c.is_whitespace() || c == ';' || c == '|')
                            })
                        })
                        .map(|r| r.len())?;
                    it.nth(n - 1);
                    word.clear();
                    has = false;
                }
                (None, '$' | '`' | '\\' | '<' | '>' | '(' | ')' | '\n') => return None,
                // A comment: what follows is never run.
                (None, '#') if !has => return None,
                (None, c) if c.is_whitespace() => {
                    if has {
                        cur.push(std::mem::take(&mut word));
                        has = false;
                    }
                }
                (None, '&' | '|' | ';') => {
                    // `&&` and `||` run the next command only on a condition: not trusted.
                    let doubled = it.peek() == Some(&c);
                    if c == '&' || (c == '|' && doubled) {
                        return None;
                    }
                    if doubled {
                        it.next();
                    }
                    if has {
                        cur.push(std::mem::take(&mut word));
                        has = false;
                    }
                    if cur.is_empty() {
                        return None;
                    }
                    out.push(std::mem::take(&mut cur));
                }
                (None, c) => (word.push(c), has = true).1,
            }
        }
        if quote.is_some() {
            return None;
        }
        if has {
            cur.push(word);
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        Some(out)
    };
    let outer = words(command)?;
    if let [w] = outer.as_slice()
        && w.len() == 3
        && ["sh", "bash", "zsh", "dash"].contains(&w[0].rsplit('/').next().unwrap_or(""))
        && ["-c", "-lc", "-ic", "-lic"].contains(&w[1].as_str())
    {
        return words(&w[2]);
    }
    Some(outer)
}

/// Did the shell string `command` (which exited 0) open a file of `t`? Only a reading program
/// with the file among its words counts; a string with any program that is neither a reader nor
/// a looker (`LOOKERS`), or that changes folder, is not trusted at all.
fn command_opens(command: &str, t: &Targets) -> bool {
    let Some(cmds) = simple_commands(command) else {
        return false;
    };
    let prog = |c: &Vec<String>| c[0].rsplit('/').next().unwrap_or("").to_string();
    if cmds
        .iter()
        .any(|c| ["cd", "pushd", "popd"].contains(&prog(c).as_str()))
    {
        return false;
    }
    if cmds
        .iter()
        .any(|c| !READERS.contains(&prog(c).as_str()) && !LOOKERS.contains(&prog(c).as_str()))
    {
        return false;
    }
    cmds.iter()
        .filter(|c| READERS.contains(&prog(c).as_str()))
        .any(|c| c[1..].iter().any(|w| t.names(w)))
}

/// `codex exec --json`: completed command executions and the last agent message.
fn read_other(out: &str, t: &Targets) -> Stream {
    let mut r = Stream::default();
    for v in out
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
    {
        let item = &v["item"];
        match (v["type"].as_str(), item["type"].as_str()) {
            (Some("item.completed"), Some("command_execution")) => {
                let command = item["command"].as_str().unwrap_or_default();
                r.seen.push(command.to_string());
                if item["exit_code"] == json!(0) && command_opens(command, t) {
                    r.reads += 1
                }
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

/// `claude -p --output-format stream-json`: a `Read` of a file (or a `Grep` naming one) whose
/// tool result is not an error, and the result.
fn read_same(out: &str, t: &Targets) -> Stream {
    let mut r = Stream::default();
    // tool_use id -> whether the call opened a file of `t`
    let mut calls: std::collections::HashMap<String, bool> = Default::default();
    for v in out
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
    {
        let blocks = v["message"]["content"].as_array().into_iter().flatten();
        match v["type"].as_str() {
            Some("assistant") => {
                for c in blocks.filter(|c| c["type"] == "tool_use") {
                    let (name, input) = (c["name"].as_str().unwrap_or("?"), &c["input"]);
                    let path = match name {
                        "Read" => input["file_path"].as_str(),
                        "Grep" => input["path"].as_str(),
                        _ => None,
                    };
                    r.seen
                        .push(format!("{name} {}", path.unwrap_or(&input.to_string())));
                    if let Some(id) = c["id"].as_str() {
                        calls.insert(id.into(), path.is_some_and(|p| t.names(p)));
                    }
                }
            }
            Some("user") => {
                r.reads += blocks
                    .filter(|c| c["type"] == "tool_result" && c["is_error"] != json!(true))
                    .filter(|c| c["tool_use_id"].as_str().and_then(|i| calls.get(i)) == Some(&true))
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

/// What the reader did, for the reason of a round without a read: the first actions, each cut,
/// the whole capped.
fn seen_text(seen: &[String]) -> String {
    if seen.is_empty() {
        return "it did nothing".into();
    }
    let cut = |s: &String| -> String {
        let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
        match one.char_indices().nth(120) {
            Some((i, _)) => format!("{}...", &one[..i]),
            None => one,
        }
    };
    let mut t = String::new();
    for (i, s) in seen.iter().enumerate() {
        if i == 8 || t.len() > 600 {
            t.push_str(&format!(" (and {} more)", seen.len() - i));
            break;
        }
        t.push_str(if i == 0 { "" } else { "; " });
        t.push_str(&cut(s));
    }
    format!("it ran: {t}")
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

/// Which reader a boundary is made for: the files of the person's home it is given differ.
#[derive(Clone, Copy, PartialEq)]
enum Reader {
    Other,
    Same,
}

/// The system folders every reader sees, read-only (when they exist).
const SYSTEM: [&str; 7] = ["/usr", "/bin", "/sbin", "/lib", "/lib64", "/lib32", "/etc"];

/// The person's home as makibad has it.
fn person_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// `p` with a leading `~` read as the person's home.
fn expand(p: &Path, home: Option<&Path>) -> Option<PathBuf> {
    match (p.strip_prefix("~"), home) {
        (Ok(rest), Some(h)) => Some(h.join(rest)),
        (Ok(_), None) => None,
        (Err(_), _) => Some(std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())),
    }
}

/// `$var`, else `<home>/<default>`: a reader's own configuration folder.
fn config_dir(var: &str, home: Option<&Path>, default: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|h| h.join(default)))
}

/// The places no reader may see, also where a folder it is given holds them: the keys, the
/// state folder, the person's credentials, the base's folders and both readers' configuration.
fn denied(places: &Places, home: Option<&Path>) -> Vec<PathBuf> {
    let (cred_dirs, cred_files) = super::prepare::credentials();
    let mut out: Vec<PathBuf> = vec![crate::cli::person_key_path(), crate::cli::gate_key_path()];
    if let Some(state) = places.state_db.parent() {
        out.push(state.to_path_buf());
    }
    out.extend(cred_dirs.into_iter().chain(cred_files));
    let base = crate::base::worker_denies();
    out.extend(base.resolved.into_iter().chain(base.folders));
    out.push(PathBuf::from("~/.claude.json"));
    out.extend(config_dir("CODEX_HOME", home, ".codex"));
    out.extend(config_dir("CLAUDE_CONFIG_DIR", home, ".claude"));
    with_real(
        out.into_iter()
            .filter_map(|p| expand(&p, home))
            .map(|p| std::path::absolute(&p).unwrap_or(p))
            .collect(),
    )
}

/// The person's home, by its path and by its real path, where it lies in a folder given whole
/// (a home under `/usr`): what the system binds would otherwise show of it.
fn home_masks(given: &[PathBuf], home: Option<&Path>) -> Vec<PathBuf> {
    let Some(h) = home else { return vec![] };
    let mut out: Vec<PathBuf> = vec![];
    for p in with_real(vec![
        std::path::absolute(h).unwrap_or_else(|_| h.to_path_buf()),
    ]) {
        if given.iter().any(|g| p.starts_with(g)) && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// `p` as the filesystem resolves it (links followed), or `p` when it does not resolve.
fn real(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Each path and, where a link leads elsewhere, the place it leads to: a denied place reached
/// through a link is denied where it really is.
fn with_real(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = vec![];
    for p in paths {
        let r = real(&p);
        for q in [p, r] {
            if !out.contains(&q) {
                out.push(q);
            }
        }
    }
    out
}

/// Where a denied place shows inside the folders given: its own path under a given folder, and,
/// when a given folder or the place is reached through a link, the path under the given folder
/// at which its real place shows. Links themselves are left out: what they lead to is covered at
/// its real place.
fn masks(given: &[PathBuf], deny: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = vec![];
    for g in given {
        let gr = real(g);
        for q in deny {
            let qr = real(q);
            let mut seen = vec![];
            if q.starts_with(g) {
                seen.push(q.clone());
            }
            if let Ok(rel) = qr.strip_prefix(&gr) {
                seen.push(g.join(rel));
            }
            for m in seen {
                let link = m
                    .symlink_metadata()
                    .is_ok_and(|md| md.file_type().is_symlink());
                if !link && !out.contains(&m) {
                    out.push(m);
                }
            }
        }
    }
    out
}

/// The files a reader's program needs to start: the program as found, the file it leads to,
/// and a script's interpreter (`#!/usr/bin/env NAME` read on makibad's PATH, or a path).
fn program_files(program: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let add = |p: PathBuf, out: &mut Vec<PathBuf>| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    let found = std::path::absolute(program).unwrap_or_else(|_| program.to_path_buf());
    add(found.clone(), &mut out);
    let target = std::fs::canonicalize(&found).unwrap_or(found);
    add(target.clone(), &mut out);
    let mut head = [0u8; 256];
    let n = std::fs::File::open(&target)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    let first = String::from_utf8_lossy(&head[..n]);
    if let Some(line) = first.strip_prefix("#!").and_then(|l| l.lines().next()) {
        let words: Vec<&str> = line.split_whitespace().collect();
        let interpreter = match words.as_slice() {
            [env, name, ..] if env.ends_with("/env") && !name.starts_with('-') => {
                agents::on_path(name)
            }
            [path, ..] if path.starts_with('/') => Some(PathBuf::from(path)),
            _ => None,
        };
        if let Some(i) = interpreter {
            add(i.clone(), &mut out);
            if let Ok(c) = std::fs::canonicalize(&i) {
                add(c, &mut out);
            }
        }
    }
    out
}

/// The folder given for a program file: its folder, or that folder's parent when it is a `bin`
/// (the install prefix); `None` when that folder is `/`, holds the home, or holds or lies in a
/// denied place: then only the file is given.
fn program_root(file: &Path, home: Option<&Path>, deny: &[PathBuf]) -> Option<PathBuf> {
    let dir = file.parent()?;
    let root = if dir.file_name().is_some_and(|n| n == "bin") {
        dir.parent()?
    } else {
        dir
    };
    (!too_wide(root, home, deny)).then(|| root.to_path_buf())
}

/// Is `root` too wide to give a reader: `/`, the home or an ancestor of it, or a folder that
/// holds or lies in a denied place (judged by its own and its real path)?
fn too_wide(root: &Path, home: Option<&Path>, deny: &[PathBuf]) -> bool {
    let rr = real(root);
    [root, rr.as_path()].iter().any(|r| {
        *r == Path::new("/")
            || home.is_some_and(|h| h.starts_with(r) || real(h).starts_with(r))
            || deny.iter().any(|q| q.starts_with(r) || r.starts_with(q))
    })
}

/// The outermost `node_modules` folder above `file`, when it is not too wide: Node looks for a
/// package's dependencies in every `node_modules` up from it, so a dependency hoisted beside the
/// package (npm's global install under a prefix the reader is not given whole) is found there.
fn node_root(file: &Path, home: Option<&Path>, deny: &[PathBuf]) -> Option<PathBuf> {
    let top = file
        .ancestors()
        .filter(|a| a.file_name().is_some_and(|n| n == "node_modules"))
        .last()?;
    (!too_wide(top, home, deny)).then(|| top.to_path_buf())
}

/// The empty file at `rel` in a reader's new home on which its login is bound: its folder is
/// made new or is a real folder (never a link), and the file is made new (`O_EXCL`), so nothing
/// already there (a link, a pipe) is followed or opened.
fn place_login(round_home: &Path, rel: &str) -> Result<PathBuf, String> {
    let dst = round_home.join(rel);
    if let Some(d) = dst.parent()
        && d != round_home
    {
        // A folder this call made for another login file is a real folder; anything else is not
        // used.
        match d.symlink_metadata() {
            Ok(m) if m.file_type().is_dir() => {}
            Ok(_) => return Err(format!("{}: not a folder", d.display())),
            Err(_) => std::fs::create_dir(d).map_err(|e| format!("{}: {e}", d.display()))?,
        }
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&dst)
        .map_err(|e| format!("{}: {e}", dst.display()))?;
    Ok(dst)
}

/// Where the entry `p` itself lies: its folder's real path and its own name, a last link not
/// followed. A link in a system folder that leads into a hidden home lies in the system folder.
fn entry(p: &Path) -> PathBuf {
    match (p.parent(), p.file_name()) {
        (Some(d), Some(n)) => real(d).join(n),
        _ => real(p),
    }
}

/// Does a folder of `given` show `p`: one holds it, and every place of `hidden` that holds `p`
/// (by its path or where the entry really lies) also holds that folder, which was then given
/// back inside the hidden place after its tmpfs (Issue 568). A system bind does not show what
/// lies under a home hidden in it.
fn shown_by(p: &Path, given: &[PathBuf], hidden: &[PathBuf]) -> bool {
    let pr = entry(p);
    given.iter().any(|g| {
        p.starts_with(g)
            && hidden
                .iter()
                .filter(|h| p.starts_with(h) || pr.starts_with(h))
                .all(|h| g.starts_with(h))
    })
}

/// The mounts of the system folders and of a reader's program (Issue 262, Issue 568), for
/// `bounded_reader`: each folder of `system` read-only (a link kept as a link), a tmpfs on a
/// `home` that lies in one, `/etc/resolv.conf` where it leads elsewhere, the program's own
/// folders and files, and the covers of the `deny` places the folders given hold. A program file
/// or folder under the hidden home is given back after its tmpfs. makibad passes `SYSTEM`; a
/// test passes a system folder of its own. It runs nothing: it reads the files' metadata, the
/// program's first bytes and makibad's `PATH` (for a script's interpreter).
#[doc(hidden)]
pub fn program_mounts(
    system: &[&Path],
    program: &Path,
    home: Option<&Path>,
    deny: &[PathBuf],
) -> Result<Vec<String>, String> {
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let mut a: Vec<String> = vec![];
    let ro = |a: &mut Vec<String>, from: &Path, to: &Path| {
        a.extend(["--ro-bind".into(), s(from), s(to)]);
    };
    // The folders given read-only before the masks: what a mask must cover.
    let mut given: Vec<PathBuf> = vec![];
    for &d in system {
        match d.symlink_metadata() {
            Err(_) => {}
            Ok(m) if m.file_type().is_symlink() => {
                let to = std::fs::read_link(d).map_err(|e| format!("{}: {e}", d.display()))?;
                a.extend(["--symlink".into(), s(&to), s(d)]);
            }
            Ok(_) => {
                ro(&mut a, d, d);
                given.push(d.to_path_buf());
            }
        }
    }
    // A home that lies in a system folder is hidden before anything of it is given back.
    let hidden = home_masks(&given, home);
    for h in &hidden {
        a.extend(["--tmpfs".into(), s(h)]);
    }
    let under_hidden = |p: &Path| {
        let r = entry(p);
        hidden.iter().any(|h| p.starts_with(h) || r.starts_with(h))
    };
    let in_system = |p: &Path| {
        let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        system.iter().any(|d| c.starts_with(d))
            && system.iter().any(|d| p.starts_with(d))
            && !under_hidden(p)
    };
    if let Ok(r) = std::fs::canonicalize("/etc/resolv.conf")
        && !in_system(&r)
    {
        ro(&mut a, &r, &r);
    }
    let mut files: Vec<PathBuf> = vec![];
    for f in program_files(program) {
        if in_system(&f) {
            continue;
        }
        let modules = node_root(&f, home, deny);
        match program_root(&f, home, deny) {
            Some(root) if !shown_by(&root, &given, &hidden) => {
                ro(&mut a, &root, &root);
                given.push(root);
            }
            Some(_) => {}
            None => files.push(f),
        }
        if let Some(nm) = modules
            && !shown_by(&nm, &given, &hidden)
        {
            ro(&mut a, &nm, &nm);
            given.push(nm);
        }
    }
    for q in masks(&given, deny) {
        match q.symlink_metadata() {
            Ok(m) if m.is_dir() => a.extend(["--tmpfs".into(), s(&q)]),
            Ok(_) => ro(&mut a, Path::new("/dev/null"), &q),
            Err(_) => {}
        }
    }
    // A program that is a link (an npm `bin` entry) stays a link to its real place, which is
    // given by its own folder or file above: bound as a file, a script would run away from the
    // package it loads its parts from.
    for f in &files {
        if f.symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            a.extend(["--symlink".into(), s(&real(f)), s(f)]);
        } else {
            ro(&mut a, f, f);
        }
    }
    Ok(a)
}

/// The reader `program` started inside the boundary (Issue 262): a command of the boundary
/// program whose arguments end with `--` and the program; the caller adds the reader's own.
/// Nothing is visible but the system folders, the program's own folders, the round's tree
/// (read-only), its scratch and settings, and a home of the round holding only the reader's
/// login (and codex's configuration, read-only) from the person's home.
fn bounded_reader(
    boundary: &Path,
    program: &Path,
    reader: Reader,
    places: &Places,
    snap: &Snapshot,
) -> Result<Command, String> {
    let home = person_home();
    let deny = denied(places, home.as_deref());
    let scratch = snap.dir.join("scratch");
    // Each reader gets a home of its own, made new here: what a first reader left in its home
    // (a link, a pipe) is never opened by makibad for the fallback.
    let round_home = snap.dir.join(match reader {
        Reader::Other => "home",
        Reader::Same => "home-fallback",
    });
    let mkdir = |d: &Path| std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()));
    mkdir(&scratch)?;
    std::fs::create_dir(&round_home).map_err(|e| format!("{}: {e}", round_home.display()))?;
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let mut a: Vec<String> = [
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--die-with-parent",
        "--new-session",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
    ]
    .map(String::from)
    .to_vec();
    let ro = |a: &mut Vec<String>, src: &Path, dst: &Path| {
        a.extend(["--ro-bind".into(), s(src), s(dst)]);
    };
    a.extend(program_mounts(
        &SYSTEM.map(Path::new),
        program,
        home.as_deref(),
        &deny,
    )?);
    ro(&mut a, &snap.tree, &snap.tree);
    a.extend(["--bind".into(), s(&scratch), s(&scratch)]);
    let settings = snap.dir.join("settings.json");
    if reader == Reader::Same && settings.exists() {
        ro(&mut a, &settings, &settings);
    }
    a.extend(["--bind".into(), s(&round_home), s(&round_home)]);
    // The reader's login, each a file of the person's, bound into the round's home.
    let logins: Vec<(PathBuf, &str, bool)> = match reader {
        Reader::Other => config_dir("CODEX_HOME", home.as_deref(), ".codex")
            .map(|d| {
                vec![
                    (d.join("auth.json"), ".codex/auth.json", true),
                    (d.join("config.toml"), ".codex/config.toml", false),
                ]
            })
            .unwrap_or_default(),
        Reader::Same => config_dir("CLAUDE_CONFIG_DIR", home.as_deref(), ".claude")
            .map(|d| {
                vec![(
                    d.join(".credentials.json"),
                    ".claude/.credentials.json",
                    true,
                )]
            })
            .unwrap_or_default(),
    };
    for (src, rel, rw) in logins {
        if !src.is_file() {
            continue;
        }
        let dst = place_login(&round_home, rel)?;
        let opt = if rw { "--bind" } else { "--ro-bind" };
        a.extend([opt.into(), s(&src), s(&dst)]);
    }
    a.extend(["--chdir".into(), s(&snap.tree), "--".into(), s(program)]);
    let mut c = Command::new(boundary);
    c.current_dir(&snap.tree)
        .args(a)
        .env("HOME", &round_home)
        .env("TMPDIR", &scratch);
    for v in [
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
    ] {
        c.env_remove(v);
    }
    Ok(c)
}

fn run_other(
    boundary: &Path,
    program: &Path,
    places: &Places,
    snap: &Snapshot,
    design: bool,
    until: Instant,
) -> Result<(Ran, Stream), String> {
    let mut c = bounded_reader(boundary, program, Reader::Other, places, snap)?;
    c.args([
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
    let ran = bounded(c, None, until.saturating_duration_since(Instant::now()))?;
    let read = read_other(&ran.out, &Targets::of(snap, design));
    Ok((ran, read))
}

fn run_same(
    boundary: &Path,
    program: &Path,
    places: &Places,
    snap: &Snapshot,
    design: bool,
    until: Instant,
) -> Result<(Ran, Stream), String> {
    let settings = same_settings(places, snap)?;
    let mut c = bounded_reader(boundary, program, Reader::Same, places, snap)?;
    c.env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1")
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
    let ran = bounded(
        c,
        Some(&prompt(design)),
        until.saturating_duration_since(Instant::now()),
    )?;
    let read = read_same(&ran.out, &Targets::of(snap, design));
    Ok((ran, read))
}

/// Judge one reader's run.
fn judge(reader: String, ran: &Ran, read: Stream) -> Answer {
    let found = findings(&read.text);
    let reason = if ran.timed_out {
        Some(format!(
            "stopped at the round's bound of {} minutes",
            BOUND.as_secs() / 60
        ))
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
        Some(format!(
            "the reader opened no file of the change under review (an empty pass); {}",
            seen_text(&read.seen)
        ))
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
/// Both readers share one deadline: the round never runs past `BOUND`, which the tool call's own
/// timeout is above.
pub fn run(plan: &Plan, places: &Places, snap: &Snapshot, design: bool) -> Answer {
    let until = Instant::now() + BOUND;
    let boundary = match &plan.boundary {
        Ok(b) => b.clone(),
        Err(why) => {
            return withheld(Answer {
                established: false,
                reader: "none".into(),
                fallback_reason: None,
                reason: Some(why.clone()),
                findings: None,
                text: String::new(),
            });
        }
    };
    let fallback_reason = match &plan.other {
        Ok(program) => match run_other(&boundary, program, places, snap, design, until) {
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
        return withheld(Answer {
            established: false,
            reader: "none".into(),
            fallback_reason: None,
            reason: Some(fallback_reason),
            findings: None,
            text: String::new(),
        });
    };
    let mut a = match run_same(&boundary, same, places, snap, design, until) {
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

/// What in `text` must not leave makibad: a token shape of the secret scanner or a key's content.
pub fn secret_in(text: &str) -> Option<String> {
    let keys: Vec<String> = [crate::cli::person_key_path(), crate::cli::gate_key_path()]
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .map(|k| k.trim().to_string())
        .filter(|k| k.len() >= 16)
        .collect();
    crate::hooks::git_hook::find(text.as_bytes())
        .map(|(shape, _)| shape.to_string())
        .or_else(|| {
            keys.iter()
                .any(|k| text.contains(k.as_str()))
                .then(|| "a makiba key".to_string())
        })
}

/// A diagnostic string as it may be returned: replaced when it carries a secret.
pub fn scrub(text: String) -> String {
    match secret_in(&text) {
        Some(shape) => format!("(withheld: it carried {shape})"),
        None => text,
    }
}

/// An answer with a secret in it comes back empty, not established; its diagnostics are scrubbed.
fn withheld(mut a: Answer) -> Answer {
    a.fallback_reason = a.fallback_reason.map(scrub);
    a.reason = a.reason.map(scrub);
    if let Some(shape) = secret_in(&a.text) {
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

    /// A launcher in the system folder that is a link into a hidden home (the review of Issue
    /// 568): the link is shown by the system bind as it is (made again, bubblewrap would refuse
    /// the link already there), and the program it leads to is given back after the home's tmpfs.
    #[test]
    fn a_system_launcher_into_a_hidden_home_stays_as_the_system_shows_it() {
        let t = std::fs::canonicalize(tempfile::tempdir().unwrap().keep()).unwrap();
        let usr = t.join("usr");
        let home = usr.join("local/home/someone");
        let program = home.join(".local/bin/codex");
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(&program, "").unwrap();
        let launcher = usr.join("local/bin/codex");
        std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink("../home/someone/.local/bin/codex", &launcher).unwrap();
        let args = program_mounts(&[usr.as_path()], &launcher, Some(&home), &[]).unwrap();
        let s = |p: &Path| p.display().to_string();
        let at = |w: &[String]| args.windows(w.len()).position(|x| x == w);
        assert!(
            !args.iter().skip(1).any(|a| *a == s(&launcher)),
            "the launcher is mounted again: {args:?}"
        );
        let local = home.join(".local");
        let tmpfs = at(&["--tmpfs".into(), s(&home)]).expect("no tmpfs on the home");
        let bind = at(&["--ro-bind".into(), s(&local), s(&local)])
            .unwrap_or_else(|| panic!("the program's folder is not given back: {args:?}"));
        assert!(tmpfs < bind, "{args:?}");
    }

    /// A reader's configuration reached through a link (the review of Issue 262): a program
    /// folder holding its real place is not given, and where a given folder holds it, it is
    /// covered at the path it shows at.
    /// A home under a system folder (the review of Issue 262): it is hidden by its path and by
    /// where it really is, and a home outside every given folder is left alone.
    #[test]
    fn a_home_in_a_system_folder_is_hidden() {
        let t = std::fs::canonicalize(tempfile::tempdir().unwrap().keep()).unwrap();
        let usr = t.join("usr");
        let home = usr.join("local/home/someone");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(t.join("home")).unwrap();
        let linked = t.join("home/someone");
        std::os::unix::fs::symlink(&home, &linked).unwrap();
        assert_eq!(
            home_masks(std::slice::from_ref(&usr), Some(&home)),
            vec![home.clone()]
        );
        assert_eq!(
            home_masks(std::slice::from_ref(&usr), Some(&linked)),
            vec![home.clone()]
        );
        assert!(home_masks(&[t.join("etc")], Some(&linked)).is_empty());
        assert!(home_masks(&[usr], None).is_empty());
    }

    #[test]
    fn a_denied_place_behind_a_link_is_denied_where_it_really_is() {
        let t = std::fs::canonicalize(tempfile::tempdir().unwrap().keep()).unwrap();
        std::fs::create_dir_all(t.join("reader/bin")).unwrap();
        std::fs::create_dir_all(t.join("reader/config/sessions")).unwrap();
        std::fs::write(t.join("reader/bin/codex"), "").unwrap();
        std::fs::create_dir_all(t.join("home")).unwrap();
        std::os::unix::fs::symlink(t.join("reader/config"), t.join("home/.codex")).unwrap();
        std::os::unix::fs::symlink(t.join("reader"), t.join("alias")).unwrap();
        let deny = with_real(vec![t.join("home/.codex")]);
        assert!(deny.contains(&t.join("reader/config")), "{deny:?}");
        let home = t.join("home");
        assert_eq!(
            program_root(&t.join("reader/bin/codex"), Some(&home), &deny),
            None
        );
        assert_eq!(
            program_root(&t.join("alias/bin/codex"), Some(&home), &deny),
            None
        );
        let m = masks(&[t.join("reader"), t.join("alias")], &deny);
        assert!(m.contains(&t.join("reader/config")), "{m:?}");
        assert!(m.contains(&t.join("alias/config")), "{m:?}");
        assert!(!m.contains(&t.join("home/.codex")), "{m:?}");
    }

    /// A reader installed by npm under a prefix that also holds the state folder (the review of
    /// Issue 262): the prefix is not given, the `bin` link stays a link to the script, and the
    /// package the script lives in is given.
    #[test]
    fn a_linked_program_stays_a_link_to_its_package() {
        let t = std::fs::canonicalize(tempfile::tempdir().unwrap().keep()).unwrap();
        let local = t.join("local");
        let package = local.join("lib/node_modules/@openai/codex");
        std::fs::create_dir_all(package.join("bin")).unwrap();
        std::fs::write(package.join("bin/codex.js"), "#!/bin/sh\n").unwrap();
        std::fs::create_dir_all(local.join("bin")).unwrap();
        let found = local.join("bin/codex");
        std::os::unix::fs::symlink("../lib/node_modules/@openai/codex/bin/codex.js", &found)
            .unwrap();
        let state = local.join("state/makiba");
        std::fs::create_dir_all(&state).unwrap();
        let places = Places {
            workers: t.join("workers"),
            docs: t.join("docs"),
            repos: t.join("repos"),
            settings: state.join("settings"),
            state_db: state.join("state.db"),
        };
        let dir = state.join("tmp/review-x");
        std::fs::create_dir_all(dir.join("tree")).unwrap();
        let snap = Snapshot {
            tree: dir.join("tree"),
            dir,
            base: String::new(),
            head: String::new(),
            changed: vec![],
            top: state.join("tmp"),
        };
        let c = bounded_reader(Path::new("bwrap"), &found, Reader::Other, &places, &snap).unwrap();
        let args: Vec<String> = c
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let has = |w: &[String]| args.windows(w.len()).any(|x| x == w);
        let s = |p: &Path| p.display().to_string();
        let target = package.join("bin/codex.js");
        assert!(
            has(&["--symlink".into(), s(&target), s(&found)]),
            "{args:?}"
        );
        assert!(
            has(&["--ro-bind".into(), s(&package), s(&package)]),
            "{args:?}"
        );
        // A dependency npm hoisted beside the package is in the outermost node_modules.
        let modules = local.join("lib/node_modules");
        assert!(
            has(&["--ro-bind".into(), s(&modules), s(&modules)]),
            "{args:?}"
        );
        assert!(
            !has(&["--ro-bind".into(), s(&local), s(&local)]),
            "{args:?}"
        );
        assert!(
            !has(&["--ro-bind".into(), s(&found), s(&found)]),
            "{args:?}"
        );
    }

    /// A program named by a relative path is kept as the absolute path it names from makibad's
    /// folder (the review of Issue 262): the reader and the boundary start in the round's tree,
    /// where the same relative path could name a file the worker committed.
    #[test]
    fn a_relative_program_is_resolved_from_makibads_folder() {
        use std::os::unix::fs::PermissionsExt;
        let here = std::env::current_dir().unwrap();
        let t = std::fs::canonicalize(tempfile::tempdir().unwrap().keep()).unwrap();
        let program = t.join("bin/bwrap");
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        // Only its metadata is read; it is never run.
        std::fs::write(&program, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        // The same file named relative to the folder the test runs in, through the root.
        let mut rel = PathBuf::from(".");
        for _ in here.components().skip(1) {
            rel.push("..");
        }
        rel.push(program.strip_prefix("/").unwrap());
        let r = resolve(&rel).unwrap_or_else(|| panic!("{} not resolved", rel.display()));
        assert!(r.is_absolute(), "{}", r.display());
        assert_eq!(std::fs::canonicalize(&r).unwrap(), program);
        assert_eq!(resolve(Path::new("./no/such/program")), None);
        assert_eq!(
            resolve(Path::new("/bin/sh")),
            Some(PathBuf::from("/bin/sh"))
        );
    }

    /// The fallback's home is not the first reader's (the review of Issue 262): a link and a pipe
    /// the first reader leaves in its home are not opened when the fallback is prepared, and the
    /// fallback's home is made new.
    #[test]
    fn the_fallback_gets_a_home_of_its_own() {
        let t = std::fs::canonicalize(tempfile::tempdir().unwrap().keep()).unwrap();
        let state = t.join("state");
        let dir = state.join("tmp/review-x");
        std::fs::create_dir_all(dir.join("tree")).unwrap();
        let places = Places {
            workers: t.join("workers"),
            docs: t.join("docs"),
            repos: t.join("repos"),
            settings: state.join("settings"),
            state_db: state.join("state.db"),
        };
        let snap = std::sync::Arc::new(Snapshot {
            tree: dir.join("tree"),
            dir: dir.clone(),
            base: String::new(),
            head: String::new(),
            changed: vec![],
            top: state.join("tmp"),
        });
        let home_of = |c: &Command| {
            c.get_envs()
                .find(|(k, _)| *k == "HOME")
                .and_then(|(_, v)| v.map(PathBuf::from))
                .unwrap()
        };
        let first = bounded_reader(
            Path::new("bwrap"),
            Path::new("/bin/sh"),
            Reader::Other,
            &places,
            &snap,
        )
        .unwrap();
        let first_home = home_of(&first);
        // What a first reader could leave: its .claude a link out of the round, a pipe in it.
        let outside = t.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let _ = std::fs::remove_dir_all(first_home.join(".claude"));
        std::os::unix::fs::symlink(&outside, first_home.join(".claude")).unwrap();
        let fifo = std::ffi::CString::new(
            outside
                .join(".credentials.json")
                .to_string_lossy()
                .as_bytes(),
        )
        .unwrap();
        // SAFETY: a path this test owns, made a pipe.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        // Placed again in that home, the login refuses at once and opens nothing.
        let (ptx, prx) = std::sync::mpsc::channel();
        let again = first_home.clone();
        std::thread::spawn(move || {
            let _ = ptx.send(place_login(&again, ".claude/.credentials.json"));
        });
        assert!(
            prx.recv_timeout(Duration::from_secs(10))
                .expect("placing a login blocked on a pipe")
                .is_err()
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let (places2, snap2) = (places.clone(), snap.clone());
        std::thread::spawn(move || {
            let r = bounded_reader(
                Path::new("bwrap"),
                Path::new("/bin/sh"),
                Reader::Same,
                &places2,
                &snap2,
            )
            .map(|c| {
                c.get_envs()
                    .find(|(k, _)| *k == "HOME")
                    .and_then(|(_, v)| v.map(PathBuf::from))
                    .unwrap()
            });
            let _ = tx.send(r);
        });
        let second_home = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("preparing the fallback blocked")
            .unwrap();
        assert_ne!(second_home, first_home);
        assert!(second_home.starts_with(&dir), "{}", second_home.display());
        assert!(
            !second_home
                .join(".claude")
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
        );
    }

    #[test]
    fn the_last_findings_line_counts() {
        assert_eq!(
            findings("FINDINGS: blocker=9 major=9 minor=9\nx\nFINDINGS: blocker=1 major=2 minor=3"),
            Some([1, 2, 3])
        );
        assert_eq!(findings("FINDINGS: none"), None);
    }

    #[test]
    fn only_a_command_that_surely_ran_opens_a_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        let tree = std::fs::canonicalize(dir.path()).unwrap();
        let t = Targets {
            files: vec![tree.join("a.txt")],
            tree,
        };
        for yes in [
            "cat a.txt",
            "cat a.txt 2>/dev/null; pwd 2>&1",
            "pwd; sed -n '1,5p' a.txt",
            "/bin/bash -lc \"nl -ba a.txt | head\"",
        ] {
            assert!(command_opens(yes, &t), "{yes}");
        }
        for no in [
            "false && cat a.txt; true",
            "true || cat a.txt",
            "true # cat a.txt",
            "exit 0; cat a.txt",
            "if false; then true; cat a.txt; fi",
            "false; cat a.txt",
            "echo a.txt",
            "cd .. ; cat a.txt",
            "cat a.txt > /dev/null",
            "cat 'a.txt 2>/dev/null'; pwd",
            "cat $f",
            "cat ../a.txt",
        ] {
            assert!(!command_opens(no, &t), "{no}");
        }
    }
}
