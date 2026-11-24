//! makibad's merge act (Issue 431, docs/design/merge-act.md section 4). The door's verb `merge`
//! commits each attempt (`merge_attempted`) with the gate's allowance before anything is sent;
//! this pass is the only writer of the attempts' outcomes. It neither judges nor watches.
//!
//! Each tick, every open attempt (one no outcome event names) without a thread gets one, as
//! `push.rs` gives a job one: the thread runs gh outside any transaction and touches no database,
//! and its outcome comes back on a channel and is written on the loop's thread, with the command's
//! end and one notice to the secretary, in one transaction; a write that fails is tried again on
//! the next tick. Unlike a push, an attempt is never sent twice:
//!
//! - An attempt of this start (`boot`) not yet started is **run** once: the pull request is read
//!   (one GraphQL query); then the REST landing bound to the head (`sha`), or on a branch that
//!   requires the merge queue the `enqueuePullRequest` mutation bound to it (`expectedHeadOid`).
//!   A readable success is `merge_done` or `merge_enqueued`; a definitive rejection is
//!   `merge_failed`; anything else is read again and recorded `merge_unknown` with what GitHub
//!   shows, never `merge_done`.
//! - Every other open attempt (one of an earlier start, or one whose run could not read the pull
//!   request after sending) is only **read**, and recorded `merge_unknown` with the actor unknown.
//!   A read that fails writes nothing and is tried again on a later tick, for up to 300 s from its
//!   first failure in this process; then the outcome is `unreadable`.
//!
//! `boot` is drawn once per process: a restart draws a new one, so every attempt an earlier start
//! left open is read, never sent. The claims about GitHub this relies on (merge-act.md section 10,
//! 1-4) are [unverified] until the first real landing.

use super::door::{self, Awaiting, MERGE_OUTCOMES};
use super::gh::{self, GhError};
use crate::db::Result;
use crate::db::writer::{NewEvent, StateWriter};
use crate::spec::AGENT_SECRETARY;
use rusqlite::params;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

/// Each gh call of the act is ended after this (gh.rs's own bound).
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// How long a read that fails is tried again before the outcome is `unreadable` (the PR watch's
/// budget, merge-act.md section 4.4).
const READ_BUDGET: Duration = Duration::from_secs(300);
/// The wait between two reads of an attempt that could not be read.
const READ_AGAIN: Duration = Duration::from_secs(5);
/// The most of gh's stderr a `merge_failed` keeps.
const KEPT_STDERR: usize = 2048;

/// REST statuses that say the landing was not carried out (merge-act.md 4.2, step 4).
const DEFINITIVE_REST: [u16; 5] = [403, 404, 405, 409, 422];
/// GraphQL error types that, with no data for the mutation, say the enqueue was not carried out.
const DEFINITIVE_GRAPHQL: [&str; 3] = ["UNPROCESSABLE", "FORBIDDEN", "NOT_FOUND"];

/// The read of the act: the pull request's node id, state, head, queue and landing.
const READ_QUERY: &str = "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){pullRequest(number:$number){id state headRefOid isMergeQueueEnabled isInMergeQueue mergeQueueEntry{id state position} mergeCommit{oid} mergedAt}}}";
/// The enqueue, bound to the judged head.
const ENQUEUE_MUTATION: &str = "mutation($id:ID!,$head:GitObjectID!){enqueuePullRequest(input:{pullRequestId:$id,expectedHeadOid:$head}){mergeQueueEntry{id state position}}}";

/// This process's start: 128 random bits, hex, drawn once. The door writes it into each attempt.
pub fn boot() -> &'static str {
    static BOOT: OnceLock<String> = OnceLock::new();
    BOOT.get_or_init(|| {
        let mut bytes = [0u8; 16];
        let read = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes));
        if read.is_err() {
            // Never the same as an earlier start's: the time and the process id.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let seed = format!("{}-{}", now.as_nanos(), std::process::id());
            return door::sha256_hex(&seed)[..32].to_string();
        }
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    })
}

/// One `merge_attempted`, as the door wrote it.
#[derive(Debug, Clone)]
struct Attempt {
    id: i64,
    repo: String,
    pr: i64,
    head: String,
    method: String,
    command: Option<i64>,
    task: Option<String>,
    boot: String,
}

/// What a thread found.
#[derive(Debug, Clone)]
enum Outcome {
    Done {
        merge_commit: Value,
        answer: Value,
    },
    Enqueued {
        entry: Value,
    },
    Failed {
        sent: bool,
        exit: Option<i32>,
        http_status: Option<u16>,
        stderr: String,
        state: Value,
        why: String,
    },
    Unknown {
        observed: &'static str,
        state: Value,
        why: String,
    },
    /// The pull request could not be read: nothing is written yet. `act`: why a run that sent
    /// cannot tell its outcome.
    NoRead {
        act: Option<String>,
        read: String,
    },
}

/// The pull request as the act reads it.
#[derive(Debug, Clone)]
struct PrRead {
    raw: Value,
    node: String,
    state: String,
    head: String,
    queue_required: Option<bool>,
    queued: bool,
}

pub struct Merge {
    /// The attempts this process ran (or began to run): never run again.
    started: HashSet<i64>,
    running: HashSet<i64>,
    /// The pull requests (`repo` lowercased, `pr`) with a thread.
    busy: HashSet<(String, i64)>,
    first_unreadable: HashMap<i64, Instant>,
    next_read: HashMap<i64, Instant>,
    /// Why a run that sent cannot tell its outcome, kept for its later reads.
    why: HashMap<i64, String>,
    tx: Sender<(Attempt, Outcome)>,
    rx: Receiver<(Attempt, Outcome)>,
}

impl Default for Merge {
    fn default() -> Self {
        Self::new()
    }
}

impl Merge {
    pub fn new() -> Merge {
        let (tx, rx) = channel();
        Merge {
            started: HashSet::new(),
            running: HashSet::new(),
            busy: HashSet::new(),
            first_unreadable: HashMap::new(),
            next_read: HashMap::new(),
            why: HashMap::new(),
            tx,
            rx,
        }
    }

    fn release(&mut self, a: &Attempt) {
        self.running.remove(&a.id);
        self.busy.remove(&(a.repo.to_lowercase(), a.pr));
    }

    /// Write what the threads found, then give every open attempt without a thread one.
    pub fn pass(&mut self, w: &StateWriter) -> Result<()> {
        while let Ok((a, o)) = self.rx.try_recv() {
            let o = match o {
                Outcome::NoRead { act, read } => {
                    if let Some(act) = act {
                        self.why.entry(a.id).or_insert(act);
                    }
                    let first = *self
                        .first_unreadable
                        .entry(a.id)
                        .or_insert_with(Instant::now);
                    if first.elapsed() < READ_BUDGET {
                        self.next_read.insert(a.id, Instant::now() + READ_AGAIN);
                        self.release(&a);
                        continue;
                    }
                    Outcome::Unknown {
                        observed: "unreadable",
                        state: Value::Null,
                        why: format!(
                            "{}; the pull request could not be read for {} s ({read})",
                            self.why_of(&a),
                            READ_BUDGET.as_secs()
                        ),
                    }
                }
                o => o,
            };
            match finish(w, &a, &o) {
                Ok(()) => {
                    self.release(&a);
                    self.first_unreadable.remove(&a.id);
                    self.next_read.remove(&a.id);
                    self.why.remove(&a.id);
                }
                Err(e) => {
                    eprintln!(
                        "makibad: merge attempt {}: cannot record its outcome: {e}",
                        a.id
                    );
                    // Tried again on the next tick.
                    let _ = self.tx.send((a, o));
                    break;
                }
            }
        }
        for a in open_attempts(w)? {
            let pair = (a.repo.to_lowercase(), a.pr);
            if self.running.contains(&a.id)
                || self.busy.contains(&pair)
                || self
                    .next_read
                    .get(&a.id)
                    .is_some_and(|t| Instant::now() < *t)
            {
                continue;
            }
            let run = a.boot == boot() && !self.started.contains(&a.id);
            if run {
                self.started.insert(a.id);
            }
            let why = self.why_of(&a);
            self.running.insert(a.id);
            self.busy.insert(pair);
            let (tx, program) = (self.tx.clone(), gh::program());
            std::thread::spawn(move || {
                let o = if run {
                    act(&program, &a)
                } else {
                    reconcile(&program, &a, why)
                };
                let _ = tx.send((a, o));
            });
        }
        Ok(())
    }

    /// Why an attempt read only cannot be sent, or why its run cannot tell its outcome.
    fn why_of(&self, a: &Attempt) -> String {
        self.why.get(&a.id).cloned().unwrap_or_else(|| {
            if a.boot == boot() {
                "makibad cannot read the answer of its call".into()
            } else {
                "makibad started again with this attempt open; it is read, never sent again".into()
            }
        })
    }
}

/// The attempts no outcome names, oldest first.
fn open_attempts(w: &StateWriter) -> Result<Vec<Attempt>> {
    let mut st = w.conn().prepare(&format!(
        "SELECT e.id, e.payload_json FROM events e WHERE e.kind = 'merge_attempted'
           AND NOT EXISTS (SELECT 1 FROM events o WHERE o.kind IN ({MERGE_OUTCOMES})
                           AND json_extract(o.payload_json, '$.attempt') = e.id)
         ORDER BY e.id"
    ))?;
    let rows = st.query_map([], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, text) = row?;
        let v: Value = text
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        out.push(Attempt {
            id,
            repo: s("repo"),
            pr: v["pr"].as_i64().unwrap_or_default(),
            head: s("head"),
            method: s("method"),
            command: v["command"].as_i64(),
            task: v["task_id"].as_str().map(String::from),
            boot: s("boot"),
        });
    }
    Ok(out)
}

/// One gh call, bounded: its stdout, or what its failure said.
fn call(program: &str, args: &[&str]) -> std::result::Result<String, GhError> {
    let never = AtomicBool::new(false);
    gh::run_bounded(program, args, None, &never, CALL_TIMEOUT, "gh")
        .map(|out| String::from_utf8_lossy(&out).into_owned())
}

fn gh_why(e: &GhError) -> String {
    match e {
        GhError::Missing => "no gh program".into(),
        GhError::Cancelled => "the call was cancelled".into(),
        GhError::Failed(f) => f.message.clone(),
    }
}

/// The act's read of the pull request.
fn read(program: &str, a: &Attempt) -> std::result::Result<PrRead, String> {
    let (owner, name) = a.repo.split_once('/').unwrap_or((&a.repo, ""));
    let out = call(
        program,
        &[
            "api",
            "graphql",
            "-f",
            &format!("query={READ_QUERY}"),
            "-f",
            &format!("owner={owner}"),
            "-f",
            &format!("name={name}"),
            "-F",
            &format!("number={}", a.pr),
        ],
    )
    .map_err(|e| gh_why(&e))?;
    let v: Value =
        serde_json::from_str(out.trim()).map_err(|e| format!("the read answered no JSON: {e}"))?;
    let p = &v["data"]["repository"]["pullRequest"];
    let text = |k: &str| p[k].as_str().map(String::from);
    let (Some(node), Some(state), Some(head)) = (text("id"), text("state"), text("headRefOid"))
    else {
        return Err(format!("the read answered {v}"));
    };
    if !["OPEN", "CLOSED", "MERGED"].contains(&state.as_str()) {
        return Err(format!("the read answered the state {state:?}"));
    }
    Ok(PrRead {
        raw: p.clone(),
        node,
        state,
        head,
        queue_required: p["isMergeQueueEnabled"].as_bool(),
        queued: p["isInMergeQueue"].as_bool() == Some(true),
    })
}

/// What GitHub shows of the attempt (merge-act.md section 4.4).
fn observed(a: &Attempt, r: &PrRead) -> &'static str {
    match r.state.as_str() {
        "MERGED" if r.head == a.head => "merged",
        "MERGED" => "merged_other_head",
        "CLOSED" => "closed",
        _ if r.queued && r.head == a.head => "enqueued",
        _ => "open",
    }
}

/// An answer that may have come after the act took effect: read again, never sent again.
fn after(program: &str, a: &Attempt, why: String) -> Outcome {
    match read(program, a) {
        Ok(r) => Outcome::Unknown {
            observed: observed(a, &r),
            state: r.raw,
            why,
        },
        Err(e) => Outcome::NoRead {
            act: Some(why),
            read: e,
        },
    }
}

/// An attempt that is only read.
fn reconcile(program: &str, a: &Attempt, why: String) -> Outcome {
    match read(program, a) {
        Ok(r) => Outcome::Unknown {
            observed: observed(a, &r),
            state: r.raw,
            why,
        },
        Err(e) => Outcome::NoRead { act: None, read: e },
    }
}

/// The state read right after a refusal, for its record; `null` when it cannot be read.
fn state_after(program: &str, a: &Attempt) -> Value {
    read(program, a).map(|r| r.raw).unwrap_or(Value::Null)
}

fn bounded(s: &str) -> String {
    let mut end = s.len().min(KEPT_STDERR);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// The HTTP status of `gh api --include`'s output, and its body.
fn http(out: &str) -> Option<(u16, Value)> {
    let first = out.lines().next()?.trim();
    let mut words = first.split_whitespace();
    if !words.next()?.starts_with("HTTP/") {
        return None;
    }
    let status = words.next()?.parse().ok()?;
    let body = out
        .find("\r\n\r\n")
        .map(|i| &out[i + 4..])
        .or_else(|| out.find("\n\n").map(|i| &out[i + 2..]))
        .unwrap_or_default();
    Some((
        status,
        serde_json::from_str(body.trim()).unwrap_or(Value::Null),
    ))
}

/// Run an attempt of this start, once.
fn act(program: &str, a: &Attempt) -> Outcome {
    let not_sent = |why: String, state: Value| Outcome::Failed {
        sent: false,
        exit: None,
        http_status: None,
        stderr: String::new(),
        state,
        why,
    };
    let r = match read(program, a) {
        Ok(r) => r,
        Err(e) => {
            return not_sent(
                format!("the pull request could not be read before sending ({e})"),
                Value::Null,
            );
        }
    };
    if r.state != "OPEN" {
        let why = format!("the pull request is {}", r.state.to_lowercase());
        return not_sent(why, r.raw);
    }
    if r.head != a.head {
        let why = format!("the pull request's head is {}, not {}", r.head, a.head);
        return not_sent(why, r.raw);
    }
    let Some(queue) = r.queue_required else {
        let why = "the read cannot tell whether the branch requires the merge queue".into();
        return not_sent(why, r.raw);
    };
    if queue {
        enqueue(program, a, &r)
    } else {
        land(program, a)
    }
}

/// The REST landing bound to the head.
fn land(program: &str, a: &Attempt) -> Outcome {
    let path = format!("repos/{}/pulls/{}/merge", a.repo, a.pr);
    let answer = call(
        program,
        &[
            "api",
            "--include",
            "-X",
            "PUT",
            &path,
            "-f",
            &format!("sha={}", a.head),
            "-f",
            &format!("merge_method={}", a.method),
        ],
    );
    match answer {
        Ok(out) => match http(&out) {
            Some((200, body)) if body["merged"] == json!(true) => Outcome::Done {
                merge_commit: body["sha"].clone(),
                answer: body["message"].clone(),
            },
            Some((status, _)) => after(
                program,
                a,
                format!("GitHub answered {status} without saying the pull request merged"),
            ),
            None => after(program, a, "GitHub's answer could not be read".into()),
        },
        Err(GhError::Failed(f)) => match http(&f.stdout) {
            Some((status, body)) if DEFINITIVE_REST.contains(&status) => {
                let message = body["message"].as_str().unwrap_or(&f.message);
                Outcome::Failed {
                    sent: true,
                    exit: f.exit,
                    http_status: Some(status),
                    stderr: bounded(&f.stderr),
                    state: state_after(program, a),
                    why: format!("{status}: {message}"),
                }
            }
            Some((status, _)) => after(program, a, format!("GitHub answered {status}")),
            None => after(
                program,
                a,
                format!("{}, with no status that can be read", f.message),
            ),
        },
        Err(e) => after(program, a, gh_why(&e)),
    }
}

/// The merge-queue enqueue bound to the head.
fn enqueue(program: &str, a: &Attempt, r: &PrRead) -> Outcome {
    let answer = call(
        program,
        &[
            "api",
            "graphql",
            "-f",
            &format!("query={ENQUEUE_MUTATION}"),
            "-f",
            &format!("id={}", r.node),
            "-f",
            &format!("head={}", a.head),
        ],
    );
    let (out, failure) = match answer {
        Ok(out) => (out, None),
        Err(GhError::Failed(f)) => (f.stdout.clone(), Some(f)),
        Err(e) => return after(program, a, gh_why(&e)),
    };
    let unreadable = || match &failure {
        Some(f) => format!("{}, with no answer that can be read", f.message),
        None => "GitHub's answer could not be read".into(),
    };
    let Ok(v) = serde_json::from_str::<Value>(out.trim()) else {
        return after(program, a, unreadable());
    };
    let errors: Vec<&Value> = v["errors"]
        .as_array()
        .map(|e| e.iter().collect())
        .unwrap_or_default();
    let data = &v["data"]["enqueuePullRequest"];
    let entry = &data["mergeQueueEntry"];
    if errors.is_empty() && entry.is_object() {
        return Outcome::Enqueued {
            entry: entry.clone(),
        };
    }
    let definitive = !errors.is_empty()
        && data.is_null()
        && errors.iter().all(|e| {
            e["type"]
                .as_str()
                .is_some_and(|t| DEFINITIVE_GRAPHQL.contains(&t))
        });
    if definitive {
        let message = errors
            .iter()
            .filter_map(|e| e["message"].as_str())
            .collect::<Vec<_>>()
            .join("; ");
        return Outcome::Failed {
            sent: true,
            exit: failure.as_ref().and_then(|f| f.exit),
            http_status: None,
            stderr: failure
                .as_ref()
                .map(|f| bounded(&f.stderr))
                .unwrap_or_default(),
            state: state_after(program, a),
            why: message,
        };
    }
    if errors.is_empty() {
        return after(program, a, unreadable());
    }
    after(
        program,
        a,
        format!(
            "GitHub answered the enqueue with errors that do not say it was refused: {}",
            errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; ")
        ),
    )
}

/// The outcome event, the command's end and the notice, together; nothing when the attempt has an
/// outcome already.
fn finish(w: &StateWriter, a: &Attempt, o: &Outcome) -> Result<()> {
    w.transaction(|w| {
        let ended: bool = w.conn().query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM events WHERE kind IN ({MERGE_OUTCOMES})
                   AND json_extract(payload_json, '$.attempt') = ?1)"
            ),
            [a.id],
            |r| r.get(0),
        )?;
        if ended {
            return Ok(());
        }
        let at = format!("{}#{} at {}", a.repo, a.pr, a.head);
        let (kind, payload, body, done) = match o {
            Outcome::Done {
                merge_commit,
                answer,
            } => {
                let short = merge_commit
                    .as_str()
                    .map_or("unknown".to_string(), |s| s[..s.len().min(7)].to_string());
                (
                    "merge_done",
                    json!({"attempt": a.id, "merge_commit": merge_commit, "answer": answer}),
                    format!(
                        "makibad merged {at} (merge commit {short}). The PR watch records the landing."
                    ),
                    true,
                )
            }
            Outcome::Enqueued { entry } => (
                "merge_enqueued",
                json!({"attempt": a.id, "entry": entry}),
                format!(
                    "makibad added {at} to the merge queue. The PR watch tells you when it lands or leaves the queue."
                ),
                true,
            ),
            Outcome::Failed {
                sent,
                exit,
                http_status,
                stderr,
                state,
                why,
            } => {
                let shown = state["state"]
                    .as_str()
                    .map_or("not read".to_string(), str::to_lowercase);
                let body = if *sent {
                    format!(
                        "makibad's merge of {at} was refused by GitHub: {why}. Nothing was retried. The pull request is {shown}. Deal with the cause, then ask again with merge."
                    )
                } else {
                    format!(
                        "makibad did not send its merge of {at}: {why}. Nothing was sent. Deal with the cause, then ask again with merge."
                    )
                };
                (
                    "merge_failed",
                    json!({"attempt": a.id, "sent": sent, "exit": exit, "http_status": http_status,
                           "stderr": stderr, "state": state, "why": why}),
                    body,
                    false,
                )
            }
            Outcome::Unknown {
                observed,
                state,
                why,
            } => (
                "merge_unknown",
                json!({"attempt": a.id, "observed": observed, "actor": "unknown", "state": state,
                       "why": why}),
                format!(
                    "makibad cannot tell whether its merge of {at} ran ({why}); GitHub shows it {observed}. Nothing was retried. If it is open, ask again with merge; if it landed, the PR watch records the landing with the actor unknown."
                ),
                false,
            ),
            Outcome::NoRead { .. } => return Ok(()),
        };
        w.append_event(&NewEvent {
            kind,
            actor: Some("makibad"),
            payload: Some(&payload),
            run_task_id: a.task.as_deref(),
            ..Default::default()
        })?;
        if let Some(command) = a.command {
            let (status, error_kind) = if done {
                ("done", None)
            } else {
                ("failed", Some("conflict"))
            };
            w.conn().execute(
                "UPDATE commands SET status = ?1, error_kind = ?2,
                   result_json = json_set(COALESCE(result_json, '{}'), '$.outcome', ?3),
                   finished_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE id = ?4 AND status = 'accepted'",
                params![status, error_kind, kind, command],
            )?;
        }
        match a.task.as_deref() {
            Some(task) => door::notice(w, AGENT_SECRETARY, task, &body)?,
            None => door::notice_untasked(w, AGENT_SECRETARY, &body)?,
        };
        // A queue act whose pull request had another head recorded after the attempt ends at once:
        // the entry was bound to the attempt's head (merge-act.md section 7).
        let queued = matches!(o, Outcome::Enqueued { .. })
            || matches!(o, Outcome::Unknown { observed, .. } if *observed == "enqueued");
        if queued {
            let moved: bool = w.conn().query_row(
                "SELECT EXISTS (SELECT 1 FROM events WHERE kind = 'fix_pushed' AND id > ?3
                   AND lower(json_extract(payload_json, '$.repo')) = lower(?1)
                   AND json_extract(payload_json, '$.pr') = ?2
                   AND json_extract(payload_json, '$.head_oid') != ?4)",
                params![a.repo, a.pr, a.id, a.head],
                |r| r.get(0),
            )?;
            if moved {
                let act = Awaiting {
                    attempt: a.id,
                    head: a.head.clone(),
                    queue: true,
                };
                door::merge_dequeued(w, a.task.as_deref().unwrap_or_default(), &a.repo, a.pr, &act)?;
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_status_line_and_the_body_are_read_from_an_included_answer() {
        let (status, body) =
            super::http("HTTP/2.0 405 Method Not Allowed\r\nA: b\r\n\r\n{\"message\":\"no\"}")
                .unwrap();
        assert_eq!(status, 405);
        assert_eq!(body["message"], "no");
        assert_eq!(
            super::http("HTTP/1.1 502 Bad Gateway\n\n<html>").unwrap().0,
            502
        );
        assert!(super::http("{\"message\":\"no\"}").is_none());
        assert!(super::http("").is_none());
    }

    #[test]
    fn boot_is_128_bits_and_stays() {
        assert_eq!(super::boot().len(), 32);
        assert_eq!(super::boot(), super::boot());
    }
}
