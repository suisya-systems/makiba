//! The person's command (spec section 12), defined with clap. The verbs other than `up`, `down`
//! and `status` (run locally by `main.rs`) talk to
//! `makibad` over the person's socket only, with the content of the person key file as its key;
//! with no file it sends no key. It never opens `state.db` (only `src/status_local.rs` does, for
//! `status`), so it uses no database module.
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

/// `$MAKIBA_GATE_KEY`, default `gate.key` beside the person key (by default
/// `${XDG_CONFIG_HOME:-~/.config}/makiba/gate.key`): the key makibad writes at start for the merge
/// gate hook alone (spec section 3). Beside the person key, so whatever moves that one moves both.
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
    /// The dispatcher's heartbeat, read from state.db without makibad.
    Status {
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
    },
    /// Close a task's run; refused before its done report unless --abandon.
    Close {
        task_id: String,
        #[arg(long)]
        abandon: bool,
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
    /// Keep the pending table of questions: add (--task, --question), escalate, withdraw, list.
    Pending {
        #[arg(value_parser = ["add", "escalate", "withdraw", "list"])]
        action: String,
        pending_id: Option<i64>,
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        question: Option<String>,
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
    /// Record that a pull request was opened or a fix pushed, and watch its CI.
    Pr {
        #[arg(value_parser = ["opened", "pushed"])]
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
    /// Save a raw note for the curator, with the four headings.
    Note {
        #[arg(long)]
        task: String,
        #[arg(long)]
        topic: String,
        text: String,
    },
    /// Record the person's decision on a pending skill candidate.
    Candidate {
        pattern: String,
        /// approved, rejected or into-<skill>.
        decision: String,
        #[arg(long)]
        reason: String,
    },
    /// Register a project and have makibad clone or fetch its GitHub repository.
    Project {
        slug: String,
        /// OWNER/NAME of a GitHub repository.
        repo: String,
    },
    /// Organization and project settings.
    Config {
        #[arg(value_parser = ["get", "set"])]
        action: String,
        key: String,
        value: Option<String>,
        /// The project of a project's setting (`artifact_language`).
        #[arg(long)]
        project: Option<String>,
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
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// The door's verb and arguments of a verb that goes over the door. Files named by the person
/// are read here (spec section 12), so `makibad` opens no path.
pub fn request(c: Command) -> Result<(&'static str, Value), String> {
    let none = |v: Option<String>| v.map_or(Value::Null, Value::String);
    Ok(match c {
        Command::Up { .. } | Command::Down { .. } | Command::Status { .. } => {
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
        } => {
            let mut args = json!({"kind": kind, "task_id": task_id, "title": title,
                                  "brief": read(&brief_file)?});
            for (k, v) in [("project", project), ("model", model), ("branch", branch)] {
                if let Some(v) = v {
                    args[k] = v.into();
                }
            }
            ("dispatch", args)
        }
        Command::Close { task_id, abandon } => {
            ("close", json!({"task_id": task_id, "abandon": abandon}))
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
        } => {
            let mut args = json!({"action": action});
            if let Some(id) = pending_id {
                args["id"] = id.into();
            }
            for (k, v) in [("task", task), ("question", question)] {
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
        Command::Push { task } => ("push", json!({"task": task})),
        Command::Project { slug, repo } => ("project", json!({"slug": slug, "repo": repo})),
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
    // The gate reads GitHub and git: it may take as long as the hook waits for it.
    let wait = if verb == "gate" {
        crate::hooks::merge_gate::GATE_WAIT
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

/// A verb that goes over the door: send it and print the answer.
pub fn main(c: Command) -> ExitCode {
    let (verb, request) = match request(c) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("makiba: usage: {e}");
            return exit(ErrorKind::Usage.exit_code());
        }
    };
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
}
