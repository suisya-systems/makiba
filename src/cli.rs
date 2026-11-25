//! The person's command (spec section 12), defined with clap. The verbs other than `up`, `down`,
//! `status` and `events` (run locally by `main.rs`) talk to
//! `makibad` over the person's socket only, with the content of the person key file as its key;
//! with no file it sends no key. It never opens `state.db` (only `src/status_local.rs` does, for
//! `status` and `events`), so it uses no database module.
//!
//! The places on disk the person's command and `makibad` share (spec section 2) are here, so the
//! daemon takes them from this module and not the other way round.

use crate::spec::{EXIT_OK, ErrorKind};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

/// The agents' port when `$MAKIBA_PORT` is not set (spec section 2).
pub const DEFAULT_PORT: u16 = 47317;

/// A request or a response line is at most this long (spec section 3).
pub const LINE_MAX: usize = 1 << 20;

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `MAKIBA_DOOR=none`: the test seam that leaves the socket off (spec section 2).
pub fn door_off() -> bool {
    std::env::var("MAKIBA_DOOR").as_deref() == Ok("none")
}

/// `$MAKIBA_DOOR`, default `${XDG_RUNTIME_DIR}/makiba/door.sock`; `None` when neither is set.
pub fn door_path() -> Option<PathBuf> {
    env_path("MAKIBA_DOOR")
        .or_else(|| env_path("XDG_RUNTIME_DIR").map(|d| d.join("makiba").join("door.sock")))
}

/// `$MAKIBA_SHARE_DIR`, default `${XDG_DATA_HOME:-~/.local/share}/makiba`: the role files.
/// Made absolute, since an agent reads it from its own folder.
pub fn share_dir() -> PathBuf {
    let dir = env_path("MAKIBA_SHARE_DIR").unwrap_or_else(|| {
        env_path("XDG_DATA_HOME")
            .or_else(|| env_path("HOME").map(|h| h.join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("makiba")
    });
    std::path::absolute(&dir).unwrap_or(dir)
}

/// `$MAKIBA_PERSON_KEY`, default `${XDG_CONFIG_HOME:-~/.config}/makiba/person.key`.
pub fn person_key_path() -> PathBuf {
    env_path("MAKIBA_PERSON_KEY").unwrap_or_else(|| {
        env_path("XDG_CONFIG_HOME")
            .or_else(|| env_path("HOME").map(|h| h.join(".config")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("makiba")
            .join("person.key")
    })
}

/// How long `makiba gate` waits for the door's acceptance gate, which reads GitHub and git.
const GATE_WAIT: Duration = Duration::from_secs(840);

/// `$MAKIBA_GATE_KEY`, default `gate.key` beside the person key (by default
/// `${XDG_CONFIG_HOME:-~/.config}/makiba/gate.key`): where a makibad before Issue 433 wrote the merge
/// gate hook's key. The key is retired (no door accepts it, nothing writes it); the path stays so
/// the redactions and deny rules still cover an old file on disk (docs/design/merge-act.md section 8).
pub fn gate_key_path() -> PathBuf {
    env_path("MAKIBA_GATE_KEY").unwrap_or_else(|| person_key_path().with_file_name("gate.key"))
}

/// `$MAKIBA_GOALS_DIR`, default `${XDG_CONFIG_HOME:-~/.config}/makiba/goals`: the goal ledgers,
/// `<owner>/<repo>.md` each (`docs/goals.md`).
pub fn goals_dir() -> PathBuf {
    env_path("MAKIBA_GOALS_DIR").unwrap_or_else(|| {
        env_path("XDG_CONFIG_HOME")
            .or_else(|| env_path("HOME").map(|h| h.join(".config")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("makiba")
            .join("goals")
    })
}

/// The configuration folder `makiba up` gives the base's core as its `XDG_CONFIG_HOME`:
/// `${XDG_CONFIG_HOME:-~/.config}/makiba/base`. The person's own configuration of the base is
/// never written.
pub fn base_config_home() -> PathBuf {
    env_path("XDG_CONFIG_HOME")
        .or_else(|| env_path("HOME").map(|h| h.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("makiba")
        .join("base")
}

/// `$MAKIBA_PORT`, default `DEFAULT_PORT`; `0` lets the system choose (tests).
pub fn port() -> Result<u16, String> {
    match std::env::var("MAKIBA_PORT") {
        Ok(v) if !v.is_empty() => v
            .parse()
            .map_err(|_| format!("MAKIBA_PORT is not a port: {v:?}")),
        _ => Ok(DEFAULT_PORT),
    }
}

/// The person key, or none when the file is absent or empty.
fn person_key() -> Option<String> {
    let key = std::fs::read_to_string(person_key_path()).ok()?;
    let key = key.trim();
    (!key.is_empty()).then(|| key.to_string())
}

/// The person's command: one subcommand per verb of spec section 12 that the person runs.
/// `makiba --help` and each verb's help come from here; `tests/cli_secretary.rs` compares it with
/// the table of section 12. `daemon`, `channel` and `hook` are not the person's verbs and are
/// matched by `main.rs` before this parser.
#[derive(Parser, Debug)]
#[command(
    name = "makiba",
    version,
    about = "An organization where the person talks to one secretary.",
    arg_required_else_help = true,
    disable_help_subcommand = true
)]
pub struct Makiba {
    #[command(subcommand)]
    pub verb: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Start the base if needed, makibad and the secretary. From a login shell.
    Up {
        #[arg(long)]
        no_secretary: bool,
    },
    /// Record the state and end the worker terminals; the secretary and the base only when asked.
    Down {
        #[arg(long)]
        secretary: bool,
        #[arg(long)]
        stop_base: bool,
    },
    /// Runs, terminals, anomalies, pending entries, PR watches and the heartbeat, from state.db.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// The events after an id, in order, read from state.db without makibad.
    Events {
        #[arg(long)]
        after: i64,
        #[arg(long)]
        limit: Option<u32>,
        /// Only the events of this task.
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Create a queued run and its first launch. --project is required for code.
    Dispatch {
        #[arg(long, value_parser = ["code", "document"])]
        kind: String,
        #[arg(long)]
        task_id: String,
        #[arg(long)]
        title: String,
        /// Read here; its content travels over the door.
        #[arg(long)]
        brief_file: PathBuf,
        #[arg(long, required_if_eq("kind", "code"))]
        project: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        /// The Issue number the task works on, so that a range does not dispatch it again.
        #[arg(long)]
        issue: Option<i64>,
    },
    /// Close a task's run; refused before its done report unless --abandon, or --limit for a
    /// merged run with a known limit the person accepted.
    Close {
        task_id: String,
        #[arg(long)]
        abandon: bool,
        /// Why the run is abandoned, or why the limit is accepted (one line).
        #[arg(long)]
        reason: Option<String>,
        /// The review finding the person accepted as a known limit (one line).
        #[arg(long)]
        limit: Option<String>,
        /// The pending entry the person answered yes for the limit.
        #[arg(long)]
        pending: Option<i64>,
    },
    /// The last lines of a worker's screen, read by makibad.
    Peek {
        task_id: String,
        #[arg(long)]
        lines: Option<u32>,
    },
    /// Queue a message; with --typed, type it into the recipient.
    Send {
        to: String,
        #[arg(required_unless_present = "file", conflicts_with = "file")]
        text: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        typed: bool,
    },
    /// Report on a task to the secretary.
    Report {
        #[arg(value_parser = ["start", "done", "blocked"])]
        state: String,
        #[arg(long)]
        task: String,
        text: String,
    },
    /// A message's full body.
    Show { message_id: i64 },
    /// Keep the pending table of questions: add (--task, --question and the context), escalate,
    /// withdraw, list. An entry is escalated only with all five parts of its context.
    Pending {
        #[arg(value_parser = ["add", "escalate", "withdraw", "list"])]
        action: String,
        pending_id: Option<i64>,
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        question: Option<String>,
        /// Why the question is asked.
        #[arg(long)]
        background: Option<String>,
        /// What yes leads to.
        #[arg(long)]
        if_yes: Option<String>,
        /// What no leads to.
        #[arg(long)]
        if_no: Option<String>,
        /// The recommendation; it may say there is none.
        #[arg(long)]
        recommend: Option<String>,
        /// The reason for the recommendation.
        #[arg(long)]
        why: Option<String>,
    },
    /// Answer a pending entry yes or no.
    Answer {
        pending_id: i64,
        #[arg(value_parser = ["yes", "no"])]
        answer: String,
        #[arg(long)]
        note: Option<String>,
        #[arg(long)]
        typed: bool,
    },
    /// Record that a pull request was opened or a fix pushed, and watch its CI; `rerun` watches
    /// the same head again after a rerun of its failed CI jobs.
    Pr {
        #[arg(value_parser = ["opened", "pushed", "rerun"])]
        state: String,
        #[arg(long)]
        repo: String,
        #[arg(long)]
        pr: i64,
        #[arg(long)]
        head: String,
        #[arg(long)]
        task: String,
    },
    /// Have makibad push a worker's branch, never forced.
    Push {
        #[arg(long)]
        task: String,
        /// The branch's test commit (full id), pushed alone before the rest (Issue 307).
        #[arg(long)]
        head: Option<String>,
    },
    /// Have makibad merge the base into a pushed worker branch and push it, never forced.
    Update {
        #[arg(long)]
        task: String,
        #[arg(long)]
        base: String,
    },
    /// Ask for a fresh secretary that resumes from the handover.
    Renew {
        /// Read here; its content travels over the door.
        #[arg(long)]
        handover_file: PathBuf,
    },
    /// Set a task's diff review allowance (a number, or unlimited), with a reason.
    Review {
        #[arg(long)]
        task: String,
        #[arg(long)]
        allowed: String,
        #[arg(long)]
        reason: String,
    },
    /// Save a raw note for the curator, with the four headings.
    Note {
        #[arg(long)]
        task: String,
        #[arg(long)]
        topic: String,
        text: String,
    },
    /// Record the person's decision on a skill candidate.
    Candidate {
        pattern: String,
        /// approved, rejected, deferred or into-<skill>.
        decision: String,
        #[arg(long)]
        reason: String,
    },
    /// Show, check or place a project's goal ledger (docs/goals.md).
    Goal {
        #[arg(value_parser = ["show", "check", "place"])]
        action: String,
        /// OWNER/NAME of the project's GitHub repository.
        repo: String,
        /// The draft for check and place: a file, or - for standard input.
        file: Option<String>,
    },
    /// Register a project and have makibad clone or fetch its GitHub repository.
    Project {
        slug: String,
        /// OWNER/NAME of a GitHub repository.
        repo: String,
        /// The network hosts its workers may reach, comma-separated (the person only; never
        /// github.com). Unset: the list stays as it is.
        #[arg(long)]
        hosts: Option<String>,
    },
    /// Organization and project settings.
    Config {
        #[arg(value_parser = ["get", "set"])]
        action: String,
        key: String,
        value: Option<String>,
        /// The project of a project's setting (`artifact_language`, `network_hosts`).
        #[arg(long)]
        project: Option<String>,
    },
    /// A range of work approved once; makibad dispatches its next Issue when a slot frees.
    /// approve shows the draft and makibad's newest read of it, and asks before it approves.
    Range {
        #[arg(value_parser = ["propose", "approve", "pause", "resume", "end", "skip"])]
        action: String,
        range_id: Option<i64>,
        /// The Issue a skip takes out of the range.
        issue: Option<i64>,
        #[arg(long)]
        project: Option<String>,
        /// Issue numbers, comma-separated.
        #[arg(long)]
        issues: Option<String>,
        /// A condition joined by AND: label:<l> no-label:<l> milestone:<m> clause:<=G<n>.
        #[arg(long)]
        takes: Option<String>,
        #[arg(long)]
        at_once: Option<i64>,
        #[arg(long)]
        budget_tasks: Option<i64>,
        /// Wall-clock hours from approval after which nothing new goes in (none: no limit).
        #[arg(long)]
        budget_hours: Option<f64>,
        /// Fixes pushed to a task's pull request after its first head before it is a stop (default 3);
        /// a clean merge of the base is no fix.
        #[arg(long)]
        budget_fixes: Option<i64>,
        /// Follow-ups found on the way: inside (the default of a proposal) or ask.
        #[arg(long, value_parser = ["inside", "ask"])]
        follow_ups: Option<String>,
        /// A candidate waiting only for range tasks with a pull request goes in for its design.
        #[arg(long)]
        design_ahead: bool,
        /// Read here; its content travels over the door.
        #[arg(long)]
        brief_head_file: Option<PathBuf>,
        #[arg(long)]
        replaces: Option<i64>,
        #[arg(long)]
        reason: Option<String>,
        /// approve without asking (the draft and its read are still printed).
        #[arg(long)]
        yes: bool,
        /// approve: the read the person looked at, sent instead of the newest read printed; a read
        /// that is not the newest is refused (`stale`).
        #[arg(long)]
        read: Option<i64>,
    },
    /// Run the acceptance gate for a pull request at a head; merges nothing.
    Gate {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        pr: i64,
        #[arg(long)]
        head: String,
    },
    /// A pending merge (Issue 432): approve prints the entry (the pull request, head and method,
    /// the gate's reason and each warning) and asks before the person's key lets it land; refuse
    /// ends it.
    Merge {
        #[arg(value_parser = ["approve", "refuse"])]
        action: String,
        /// The pending entry's id (`makiba status` lists it).
        pending_id: i64,
        /// refuse: why, for the secretary.
        #[arg(long)]
        reason: Option<String>,
        /// approve without asking (the entry is still printed).
        #[arg(long)]
        yes: bool,
    },
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// The door's verb and arguments of a verb that goes over the door. Files named by the person
/// are read here (spec section 12), so `makibad` opens no path.
pub fn request(c: Command) -> Result<(&'static str, Value), String> {
    let none = |v: Option<String>| v.map_or(Value::Null, Value::String);
    Ok(match c {
        Command::Up { .. }
        | Command::Down { .. }
        | Command::Status { .. }
        | Command::Events { .. } => {
            return Err("this verb runs locally, not over the door".into());
        }
        Command::Dispatch {
            kind,
            task_id,
            title,
            brief_file,
            project,
            model,
            branch,
            issue,
        } => {
            let mut args = json!({"kind": kind, "task_id": task_id, "title": title,
                                  "brief": read(&brief_file)?});
            for (k, v) in [("project", project), ("model", model), ("branch", branch)] {
                if let Some(v) = v {
                    args[k] = v.into();
                }
            }
            if let Some(n) = issue {
                args["issue"] = n.into();
            }
            ("dispatch", args)
        }
        Command::Range {
            action,
            range_id,
            issue,
            project,
            issues,
            takes,
            at_once,
            budget_tasks,
            budget_hours,
            budget_fixes,
            follow_ups,
            design_ahead,
            brief_head_file,
            replaces,
            reason,
            yes: _,
            read: shown_read,
        } => {
            let mut args = json!({"action": action});
            if design_ahead {
                args["design_ahead"] = true.into();
            }
            if let Some(id) = range_id {
                args["id"] = id.into();
            }
            if let Some(n) = issue {
                args["issue"] = n.into();
            }
            if let Some(list) = issues {
                let numbers: Vec<i64> = list
                    .split(',')
                    .map(|n| n.trim().trim_start_matches('#').parse())
                    .collect::<Result<_, _>>()
                    .map_err(|_| format!("--issues is Issue numbers, comma-separated: {list:?}"))?;
                args["issues"] = json!(numbers);
            }
            if let Some(f) = brief_head_file {
                args["brief_head"] = read(&f)?.into();
            }
            for (k, v) in [
                ("project", project),
                ("takes", takes),
                ("reason", reason),
                ("follow_ups", follow_ups),
            ] {
                if let Some(v) = v {
                    args[k] = v.into();
                }
            }
            for (k, v) in [
                ("at_once", at_once),
                ("budget_tasks", budget_tasks),
                ("budget_fixes", budget_fixes),
                ("replaces", replaces),
                ("read", shown_read),
            ] {
                if let Some(v) = v {
                    args[k] = v.into();
                }
            }
            if let Some(h) = budget_hours {
                // NaN and infinity have no JSON form: sent as null they would read as "no limit".
                if !h.is_finite() || h <= 0.0 {
                    return Err(format!(
                        "--budget-hours is a number of hours above 0, not {h}"
                    ));
                }
                args["budget_hours"] = h.into();
            }
            ("range", args)
        }
        Command::Close {
            task_id,
            abandon,
            reason,
            limit,
            pending,
        } => {
            let mut args = json!({"task_id": task_id, "abandon": abandon});
            for (name, value) in [("reason", reason), ("limit", limit)] {
                if let Some(v) = value {
                    args[name] = v.into();
                }
            }
            if let Some(p) = pending {
                args["pending"] = p.into();
            }
            ("close", args)
        }
        Command::Peek { task_id, lines } => {
            let mut args = json!({"task_id": task_id});
            if let Some(n) = lines {
                args["lines"] = n.into();
            }
            ("peek", args)
        }
        Command::Send {
            to,
            text,
            file,
            typed,
        } => {
            let text = match (text, file) {
                (_, Some(f)) => read(&f)?,
                (Some(t), None) => t,
                (None, None) => return Err("send needs a text or --file".into()),
            };
            ("send", json!({"to": to, "text": text, "typed": typed}))
        }
        Command::Report { state, task, text } => (
            "report",
            json!({"state": state, "task": task, "text": text}),
        ),
        Command::Show { message_id } => ("show", json!({"id": message_id})),
        Command::Pending {
            action,
            pending_id,
            task,
            question,
            background,
            if_yes,
            if_no,
            recommend,
            why,
        } => {
            let mut args = json!({"action": action});
            if let Some(id) = pending_id {
                args["id"] = id.into();
            }
            for (k, v) in [
                ("task", task),
                ("question", question),
                ("background", background),
                ("if_yes", if_yes),
                ("if_no", if_no),
                ("recommend", recommend),
                ("why", why),
            ] {
                if let Some(v) = v {
                    args[k] = v.into();
                }
            }
            ("pending", args)
        }
        Command::Answer {
            pending_id,
            answer,
            note,
            typed,
        } => (
            "answer",
            json!({"id": pending_id, "answer": answer, "note": none(note), "typed": typed}),
        ),
        Command::Pr {
            state,
            repo,
            pr,
            head,
            task,
        } => (
            "pr",
            json!({"state": state, "repo": repo, "pr": pr, "head": head, "task": task}),
        ),
        Command::Push { task, head: None } => ("push", json!({"task": task})),
        Command::Push {
            task,
            head: Some(head),
        } => ("push", json!({"task": task, "head": head})),
        Command::Review {
            task,
            allowed,
            reason,
        } => {
            let allowed = allowed
                .parse::<i64>()
                .map_or_else(|_| json!(allowed), |n| json!(n));
            (
                "review",
                json!({"task": task, "allowed": allowed, "reason": reason}),
            )
        }
        Command::Project { slug, repo, hosts } => {
            let mut args = json!({"slug": slug, "repo": repo});
            if let Some(h) = hosts {
                args["hosts"] = h.into();
            }
            ("project", args)
        }
        Command::Update { task, base } => ("update", json!({"task": task, "base": base})),
        Command::Renew { handover_file } => ("renew", json!({"handover": read(&handover_file)?})),
        Command::Note { task, topic, text } => {
            ("note", json!({"task": task, "topic": topic, "text": text}))
        }
        Command::Candidate {
            pattern,
            decision,
            reason,
        } => (
            "candidate",
            json!({"pattern": pattern, "decision": decision, "reason": reason}),
        ),
        Command::Goal { action, repo, file } => {
            let mut args = json!({"action": action, "repo": repo});
            if let Some(f) = file {
                args["text"] = if f == "-" {
                    let mut t = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut t)
                        .map_err(|e| format!("cannot read standard input: {e}"))?;
                    t
                } else {
                    read(Path::new(&f))?
                }
                .into();
            } else if action != "show" {
                return Err(format!(
                    "{action} needs the draft: a file, or - for standard input"
                ));
            }
            ("goal", args)
        }
        Command::Config {
            action,
            key,
            value,
            project,
        } => (
            "config",
            json!({"action": action, "key": key, "value": none(value), "project": none(project)}),
        ),
        Command::Gate { repo, pr, head } => ("gate", json!({"repo": repo, "pr": pr, "head": head})),
        Command::Merge {
            action,
            pending_id,
            reason,
            ..
        } => {
            let mut args = json!({"action": action, "id": pending_id});
            if let Some(r) = reason {
                args["reason"] = r.into();
            }
            ("merge", args)
        }
    })
}

/// Send one request to the door and read its answer line.
fn ask(verb: &str, args: Value) -> Result<Value, (ErrorKind, String)> {
    let unavailable = |e: String| (ErrorKind::Unavailable, e);
    if door_off() {
        return Err(unavailable(
            "MAKIBA_DOOR=none: the door's socket is off, so there is nothing to call".into(),
        ));
    }
    let path = door_path().ok_or_else(|| {
        unavailable("neither MAKIBA_DOOR nor XDG_RUNTIME_DIR is set: no door to call".into())
    })?;
    let mut req = json!({"v": 1, "verb": verb, "args": args});
    if let Some(key) = person_key() {
        req["key"] = Value::String(key);
    }
    let mut s = UnixStream::connect(&path)
        .map_err(|e| unavailable(format!("makibad is not running ({}: {e})", path.display())))?;
    let io = |e: std::io::Error| unavailable(format!("the door: {e}"));
    // The gate reads GitHub and git: it may take minutes.
    let wait = if verb == "gate" {
        GATE_WAIT
    } else {
        Duration::from_secs(30)
    };
    s.set_read_timeout(Some(wait)).map_err(io)?;
    writeln!(s, "{req}").map_err(io)?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).map_err(io)?;
    serde_json::from_str(&line).map_err(|e| {
        (
            ErrorKind::Internal,
            format!("the door answered {line:?}: {e}"),
        )
    })
}

pub fn exit(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// Before `range approve`: print the draft and `makibad`'s newest complete read of it, read from
/// `state.db` as `status` reads it, with its id, and ask on the terminal unless `yes`. The read's
/// id goes with the approval (or the read named with `--read`, Issue 408), so a read made since is
/// refused (`stale`) instead of approved unseen.
fn approve_preview(id: i64, yes: bool) -> Result<i64, String> {
    let status = crate::status_local::read_here()?;
    let range = status["ranges"]
        .as_array()
        .and_then(|a| a.iter().find(|r| r["id"] == json!(id)))
        .ok_or_else(|| format!("no range {id} in state.db"))?;
    let read = range["read_id"].as_i64().ok_or_else(|| {
        format!("range {id} has no complete read yet; wait for makibad to read it")
    })?;
    let text = |k: &str| range[k].as_str().unwrap_or("-").to_string();
    println!("Range {id} of {} ({})", text("project"), text("status"));
    println!("Issues: {}", range["issues"]);
    println!("Takes: {}", text("takes"));
    println!(
        "At once: {}; tasks in all: {}",
        range["at_once"], range["budget"]["tasks"]
    );
    println!(
        "Hours from approval: {}; fixes of a task's pull request before it asks: {}",
        range["budget"]["hours"]
            .as_f64()
            .map_or("no limit".to_string(), |h| h.to_string()),
        range["budget"]["fixes"]
    );
    println!(
        "Follow-ups found on the way: {}",
        match range["follow_ups"].as_str() {
            Some("inside") =>
                "inside (taken from the budget, unless they need a host or name a stop line)",
            Some("ask") => "ask (each one is a new range)",
            _ => "-",
        }
    );
    println!(
        "Design ahead: {}",
        if range["design_ahead"] == json!(true) {
            "yes (a candidate waiting for range tasks with a pull request goes in for its design only)"
        } else {
            "no"
        }
    );
    // Issue 383: makibad's mode, the one the gate reads; this shell's only beside it.
    let mode = status["merge_approval_mode"].as_str().unwrap_or("unknown");
    let here = status["merge_approval_mode_here"].as_str().unwrap_or("-");
    if mode == here {
        println!("Merge approval mode: {mode} (makibad's)");
    } else {
        println!("Merge approval mode: {mode} (makibad's); this shell says {here}");
    }
    println!("Brief head:\n{}", text("brief_head"));
    println!(
        "Read {read} at {} - in the order they would go in:",
        text("read_at")
    );
    for c in range["candidates"].as_array().into_iter().flatten() {
        let waits: Vec<String> = c["design_only_waits"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|n| format!("#{n}"))
            .collect();
        let what = if c["admitted"] == json!(true) && !waits.is_empty() {
            format!(
                "admitted for its design only, waits for {}",
                waits.join(", ")
            )
        } else if c["admitted"] == json!(true) {
            "admitted".to_string()
        } else {
            format!("left out: {}", c["left_out_why"].as_str().unwrap_or("-"))
        };
        println!(
            "  #{} {} - {what}",
            c["issue"],
            c["title"].as_str().unwrap_or("")
        );
    }
    if yes {
        return Ok(read);
    }
    print!("Approve range {id}? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|e| format!("cannot read the answer: {e}"))?;
    if answer.trim().eq_ignore_ascii_case("y") {
        Ok(read)
    } else {
        Err("not approved".into())
    }
}

/// Before `merge approve` (Issue 432): print the pending merge entry's text as `makibad` wrote it,
/// read from `state.db`, and `makibad`'s mode beside it, and ask on the terminal unless `yes`. The
/// SHA-256 of the text printed goes with the approval, so an entry changed since is refused
/// (`stale`) instead of approved unseen.
fn merge_preview(id: i64, yes: bool) -> Result<String, String> {
    let (text, shown) = crate::status_local::merge_entry_here(id)?;
    print!("{text}");
    let status = crate::status_local::read_here()?;
    let mode = status["merge_approval_mode"].as_str().unwrap_or("unknown");
    println!("Merge approval mode: {mode} (makibad's)");
    if yes {
        return Ok(shown);
    }
    print!("Approve the merge of entry {id}? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|e| format!("cannot read the answer: {e}"))?;
    if answer.trim().eq_ignore_ascii_case("y") {
        Ok(shown)
    } else {
        Err("not approved".into())
    }
}

/// A verb that goes over the door: send it and print the answer.
pub fn main(c: Command) -> ExitCode {
    let merge_preview_of = match &c {
        Command::Merge {
            action,
            pending_id,
            yes,
            ..
        } if action == "approve" => Some((*pending_id, *yes)),
        _ => None,
    };
    let preview = match &c {
        Command::Range {
            action,
            range_id,
            yes,
            ..
        } if action == "approve" => Some(((*range_id).ok_or("approve names a range id"), *yes)),
        _ => None,
    };
    let (verb, mut request) = match request(c) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("makiba: usage: {e}");
            return exit(ErrorKind::Usage.exit_code());
        }
    };
    if let Some((id, yes)) = merge_preview_of {
        match merge_preview(id, yes) {
            Ok(shown) => request["shown"] = shown.into(),
            Err(e) => {
                eprintln!("makiba: {e}");
                return exit(ErrorKind::Conflict.exit_code());
            }
        }
    }
    if let Some((id, yes)) = preview {
        match id
            .map_err(String::from)
            .and_then(|id| approve_preview(id, yes))
        {
            // A read named with `--read` goes as named; else the newest read printed.
            Ok(read) if request["read"].is_null() => request["read"] = read.into(),
            Ok(_) => {}
            Err(e) => {
                eprintln!("makiba: {e}");
                return exit(ErrorKind::Conflict.exit_code());
            }
        }
    }
    match ask(verb, request) {
        Ok(answer) if answer["ok"] == json!(true) => {
            println!("{}", answer["result"]);
            exit(EXIT_OK)
        }
        Ok(answer) => {
            let e = &answer["error"];
            let kind = e["kind"].as_str().unwrap_or("internal");
            eprintln!(
                "makiba: {kind}: {}",
                e["message"].as_str().unwrap_or_default()
            );
            if let Some(hint) = e["hint"].as_str().filter(|h| !h.is_empty()) {
                eprintln!("{hint}");
            }
            // Issue 340: a makibad of another build may not know what this makiba asks.
            if let Some(other) = crate::build::other_build_line(&answer) {
                eprintln!("{other}");
            }
            let code = ErrorKind::ALL
                .iter()
                .find(|k| k.as_str() == kind)
                .map_or(ErrorKind::Internal.exit_code(), |k| k.exit_code());
            exit(code)
        }
        Err((kind, message)) => {
            eprintln!("makiba: {}: {message}", kind.as_str());
            exit(kind.exit_code())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(a: &[&str]) -> Result<(&'static str, Value), String> {
        let m = Makiba::try_parse_from(std::iter::once("makiba").chain(a.iter().copied()))
            .map_err(|e| e.to_string())?;
        request(m.verb)
    }

    /// `dispatch` reads the brief file itself and sends its content as `brief`.
    #[test]
    fn dispatch_sends_the_files_content_and_needs_its_four_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("b.md");
        std::fs::write(&f, "do it\n").unwrap();
        let f = f.to_str().unwrap();
        let (verb, v) = req(&[
            "dispatch",
            "--kind",
            "code",
            "--task-id",
            "t",
            "--title",
            "T",
            "--brief-file",
            f,
            "--project",
            "p",
            "--model",
            "m",
            "--branch",
            "b",
        ])
        .unwrap();
        assert_eq!(verb, "dispatch");
        assert_eq!(
            v,
            json!({"kind": "code", "task_id": "t", "title": "T", "brief": "do it\n",
                   "project": "p", "model": "m", "branch": "b"})
        );
        assert!(req(&["dispatch", "--kind", "code", "--task-id", "t"]).is_err());
        assert!(req(&["dispatch", "--kind"]).is_err());
        assert!(req(&["dispatch", "--bogus", "x"]).is_err());
    }

    /// `pr` sends its four flags, the PR number as a number, and refuses what is missing.
    #[test]
    fn pr_sends_its_flags_and_needs_all_four() {
        let head = "a".repeat(40);
        let (verb, v) = req(&[
            "pr", "pushed", "--repo", "o/n", "--pr", "7", "--head", &head, "--task", "t",
        ])
        .unwrap();
        assert_eq!(verb, "pr");
        assert_eq!(
            v,
            json!({"state": "pushed", "repo": "o/n", "pr": 7, "head": head, "task": "t"})
        );
        assert!(req(&["pr", "opened", "--repo", "o/n"]).is_err());
        assert!(req(&["pr", "bogus", "--repo", "o/n"]).is_err());
        assert!(req(&["pr", "opened", "--pr", "x"]).is_err());
    }

    /// `range propose` sends the budgets; hours that are no finite number above 0 are refused
    /// here, since NaN and infinity would travel as null and read as "no limit".
    #[test]
    fn range_propose_sends_the_budgets_and_refuses_hours_that_are_no_number() {
        let propose = |more: &[&str]| {
            let mut a = vec!["range", "propose", "--project", "p", "--issues", "1,2"];
            a.extend_from_slice(more);
            req(&a)
        };
        let (verb, v) = propose(&["--budget-hours", "1.5", "--budget-fixes", "2"]).unwrap();
        assert_eq!(verb, "range");
        assert_eq!(v["budget_hours"], json!(1.5));
        assert_eq!(v["budget_fixes"], json!(2));
        let (_, v) = propose(&[]).unwrap();
        assert!(v.get("budget_hours").is_none() && v.get("budget_fixes").is_none());
        // Neither follow_ups nor design_ahead unless named: makibad's defaults hold (Issue 284).
        assert!(v.get("follow_ups").is_none() && v.get("design_ahead").is_none());
        let (_, v) = propose(&["--follow-ups", "ask", "--design-ahead"]).unwrap();
        assert_eq!(v["follow_ups"], json!("ask"));
        assert_eq!(v["design_ahead"], json!(true));
        for bad in ["NaN", "inf", "-inf", "0", "-1"] {
            let flag = format!("--budget-hours={bad}");
            assert!(propose(&[&flag]).is_err(), "{bad}");
        }
    }

    /// `merge` (Issue 432) sends the action and the entry's id, and refuse its reason; an action
    /// other than approve and refuse, or no id, is refused here.
    #[test]
    fn merge_sends_its_action_and_the_entrys_id() {
        assert_eq!(
            req(&["merge", "approve", "4", "--yes"]).unwrap(),
            ("merge", json!({"action": "approve", "id": 4}))
        );
        assert_eq!(
            req(&["merge", "refuse", "4", "--reason", "not now"]).unwrap(),
            (
                "merge",
                json!({"action": "refuse", "id": 4, "reason": "not now"})
            )
        );
        assert!(req(&["merge", "request", "4"]).is_err());
        assert!(req(&["merge", "approve"]).is_err());
    }
}
