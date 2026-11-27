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
//! The dequeue of an `UNMERGEABLE` queue entry (Issue 575, docs/design/queue-unmergeable.md) has the
//! same rule: each `queue_dequeue_requested` the PR watch wrote is run once by the start that wrote
//! it (read; send `dequeuePullRequest` only while GitHub shows the same entry `UNMERGEABLE` at the
//! request's head, after reading the whole queue of its base for the entries ahead), and only read
//! by any other start; its outcome is one `queue_dequeue_ended`, with `merge_dequeued` (`why`
//! `unmergeable`) and the PR watch's wait for a dequeue that took effect while the act or stay is
//! still the pull request's.
//!
//! `boot` is drawn once per process: a restart draws a new one, so every attempt an earlier start
//! left open is read, never sent. The claims about GitHub this relies on (merge-act.md section 10,
//! 1-4) are [unverified] until the first real landing.

use super::door::{self, Awaiting, MERGE_OUTCOMES};
use super::gh::{self, GhError};
use super::prwatch;
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
    /// The dequeue requests (Issue 575).
    dq: Dequeues,
    dq_tx: Sender<(Request, DqOutcome)>,
    dq_rx: Receiver<(Request, DqOutcome)>,
}

impl Default for Merge {
    fn default() -> Self {
        Self::new()
    }
}

impl Merge {
    pub fn new() -> Merge {
        let (tx, rx) = channel();
        let (dq_tx, dq_rx) = channel();
        Merge {
            dq: Dequeues::default(),
            dq_tx,
            dq_rx,
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
        self.dequeues(w)
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
                door::merge_dequeued(
                    w,
                    a.task.as_deref().unwrap_or_default(),
                    &a.repo,
                    a.pr,
                    &act,
                    "head recorded",
                )?;
            }
        }
        Ok(())
    })
}

// ---- the dequeue of an UNMERGEABLE entry (Issue 575, docs/design/queue-unmergeable.md) ---------

/// The read before a dequeue: the pull request's node id, state, head, base and queue entry.
const DEQUEUE_READ: &str = "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){pullRequest(number:$number){id state headRefOid baseRefName isInMergeQueue mergeQueueEntry{id state position}}}}";
/// The queue of a base, one page.
const QUEUE_READ: &str = "query($owner:String!,$name:String!,$branch:String!,$cursor:String){repository(owner:$owner,name:$name){mergeQueue(branch:$branch){entries(first:100,after:$cursor){pageInfo{hasNextPage endCursor} nodes{position pullRequest{number}}}}}}";
/// The most pages of a queue read: a longer queue is never taken as read whole.
const QUEUE_PAGES: usize = 10;
/// The dequeue of a pull request ([unverified]: the input `id` is the pull request's node id).
const DEQUEUE_MUTATION: &str =
    "mutation($id:ID!){dequeuePullRequest(input:{id:$id}){mergeQueueEntry{id}}}";

/// One `queue_dequeue_requested`, as the PR watch wrote it.
#[derive(Debug, Clone)]
struct Request {
    id: i64,
    repo: String,
    pr: i64,
    head: String,
    task: String,
    key: prwatch::DequeueKey,
    entry_id: Option<String>,
    boot: String,
}

/// The pull request as the dequeue reads it.
#[derive(Debug, Clone)]
struct DqRead {
    raw: Value,
    node: String,
    state: String,
    head: String,
    base: String,
    queued: bool,
    entry_id: Option<String>,
    entry_state: Option<String>,
}

/// What a dequeue thread found.
#[derive(Debug, Clone)]
enum DqOutcome {
    /// `dequeued`, `not_sent`, `failed`, `unknown` or `moot`.
    Ended {
        outcome: &'static str,
        ahead: Vec<i64>,
        state: Value,
        why: String,
    },
    /// Nothing could be read; `sent`: whether the mutation was sent (then it is never sent again).
    NoRead {
        sent: bool,
        ahead: Option<Vec<i64>>,
        why: String,
    },
}

/// The dequeue requests of the merge pass: run once by the start that wrote them, else only read.
#[derive(Default)]
struct Dequeues {
    running: HashSet<i64>,
    /// The requests whose mutation this process sent: never sent again.
    sent: HashSet<i64>,
    /// The entries ahead read before a send whose answer could not be read.
    ahead: HashMap<i64, Vec<i64>>,
    first_unreadable: HashMap<i64, Instant>,
    next_read: HashMap<i64, Instant>,
}

impl Merge {
    /// Write what the dequeue threads found, then give every open request without a thread one.
    fn dequeues(&mut self, w: &StateWriter) -> Result<()> {
        while let Ok((r, o)) = self.dq_rx.try_recv() {
            let o = match o {
                DqOutcome::NoRead { sent, ahead, why } => {
                    if sent {
                        self.dq.sent.insert(r.id);
                    }
                    if let Some(a) = ahead {
                        self.dq.ahead.insert(r.id, a);
                    }
                    let first = *self
                        .dq
                        .first_unreadable
                        .entry(r.id)
                        .or_insert_with(Instant::now);
                    if first.elapsed() < READ_BUDGET {
                        self.dq.next_read.insert(r.id, Instant::now() + READ_AGAIN);
                        self.dq.running.remove(&r.id);
                        continue;
                    }
                    DqOutcome::Ended {
                        outcome: "unreadable",
                        ahead: Vec::new(),
                        state: Value::Null,
                        why: format!(
                            "the pull request could not be read for {} s ({why})",
                            READ_BUDGET.as_secs()
                        ),
                    }
                }
                o => o,
            };
            match finish_dequeue(w, &r, &o) {
                Ok(()) => {
                    self.dq.running.remove(&r.id);
                    self.dq.first_unreadable.remove(&r.id);
                    self.dq.next_read.remove(&r.id);
                    self.dq.ahead.remove(&r.id);
                }
                Err(e) => {
                    eprintln!(
                        "makibad: dequeue request {}: cannot record its outcome: {e}",
                        r.id
                    );
                    let _ = self.dq_tx.send((r, o));
                    break;
                }
            }
        }
        for r in open_requests(w)? {
            if self.dq.running.contains(&r.id)
                || self
                    .dq
                    .next_read
                    .get(&r.id)
                    .is_some_and(|t| Instant::now() < *t)
            {
                continue;
            }
            // Sent only by the start that wrote the request, and only while this process has
            // not sent it: every other request is read.
            let run = r.boot == boot() && !self.dq.sent.contains(&r.id);
            let known = self.dq.ahead.get(&r.id).cloned();
            self.dq.running.insert(r.id);
            let (tx, program) = (self.dq_tx.clone(), gh::program());
            std::thread::spawn(move || {
                let o = if run {
                    dequeue(&program, &r)
                } else {
                    dequeue_read_only(&program, &r, known)
                };
                let _ = tx.send((r, o));
            });
        }
        Ok(())
    }
}

/// The requests no `queue_dequeue_ended` names, oldest first.
fn open_requests(w: &StateWriter) -> Result<Vec<Request>> {
    let mut st = w.conn().prepare(
        "SELECT e.id, e.payload_json FROM events e WHERE e.kind = 'queue_dequeue_requested'
           AND NOT EXISTS (SELECT 1 FROM events o WHERE o.kind = 'queue_dequeue_ended'
                           AND json_extract(o.payload_json, '$.request') = e.id)
         ORDER BY e.id",
    )?;
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
        let key = match (v["attempt"].as_i64(), v["stay"].as_i64()) {
            (Some(a), _) => prwatch::DequeueKey::Act(a),
            (None, Some(s)) => prwatch::DequeueKey::Stay(s),
            (None, None) => continue,
        };
        out.push(Request {
            id,
            repo: s("repo"),
            pr: v["pr"].as_i64().unwrap_or_default(),
            head: s("head"),
            task: s("task_id"),
            key,
            entry_id: v["entry"]["id"].as_str().map(String::from),
            boot: s("boot"),
        });
    }
    Ok(out)
}

fn owner_name(repo: &str) -> (&str, &str) {
    repo.split_once('/').unwrap_or((repo, ""))
}

/// The read before (and after) a dequeue.
fn dq_read(program: &str, r: &Request) -> std::result::Result<DqRead, String> {
    let (owner, name) = owner_name(&r.repo);
    let out = call(
        program,
        &[
            "api",
            "graphql",
            "--hostname",
            "github.com",
            "-f",
            &format!("query={DEQUEUE_READ}"),
            "-f",
            &format!("owner={owner}"),
            "-f",
            &format!("name={name}"),
            "-F",
            &format!("number={}", r.pr),
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
    let entry = &p["mergeQueueEntry"];
    Ok(DqRead {
        raw: p.clone(),
        node,
        state,
        head,
        base: text("baseRefName").unwrap_or_default(),
        queued: p["isInMergeQueue"].as_bool() == Some(true),
        entry_id: entry["id"].as_str().map(String::from),
        entry_state: entry["state"].as_str().map(String::from),
    })
}

/// Why a queue read gave no whole list.
enum QueueErr {
    Read(String),
    /// A queue makibad cannot take as every entry ahead: longer than it reads, or an entry
    /// without a readable position or pull request (GitHub's fields are nullable). Why.
    Incomplete(String),
}

/// Every entry of the queue of `base`: (position, number), page after page.
fn queue_entries(
    program: &str,
    r: &Request,
    base: &str,
) -> std::result::Result<Vec<(i64, i64)>, QueueErr> {
    let (owner, name) = owner_name(&r.repo);
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..QUEUE_PAGES {
        let mut args = vec![
            "api".to_string(),
            "graphql".into(),
            "--hostname".into(),
            "github.com".into(),
            "-f".into(),
            format!("query={QUEUE_READ}"),
            "-f".into(),
            format!("owner={owner}"),
            "-f".into(),
            format!("name={name}"),
            "-f".into(),
            format!("branch={base}"),
        ];
        if let Some(c) = &cursor {
            args.push("-f".into());
            args.push(format!("cursor={c}"));
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let text = call(program, &args).map_err(|e| QueueErr::Read(gh_why(&e)))?;
        let v: Value = serde_json::from_str(text.trim())
            .map_err(|e| QueueErr::Read(format!("the queue read answered no JSON: {e}")))?;
        let queue = &v["data"]["repository"]["mergeQueue"];
        if queue.is_null() && v["data"]["repository"].is_object() {
            return Ok(out);
        }
        let entries = &queue["entries"];
        let Some(nodes) = entries["nodes"].as_array() else {
            return Err(QueueErr::Read(format!("the queue read answered {v}")));
        };
        for n in nodes {
            let (Some(p), Some(number)) =
                (n["position"].as_i64(), n["pullRequest"]["number"].as_i64())
            else {
                return Err(QueueErr::Incomplete(format!(
                    "an entry of the merge queue has no readable position or pull request ({n})"
                )));
            };
            out.push((p, number));
        }
        if entries["pageInfo"]["hasNextPage"].as_bool() != Some(true) {
            return Ok(out);
        }
        cursor = entries["pageInfo"]["endCursor"].as_str().map(String::from);
        if cursor.is_none() {
            return Err(QueueErr::Read("the queue read gave no cursor".into()));
        }
    }
    Err(QueueErr::Incomplete(format!(
        "the merge queue is longer than makibad reads ({QUEUE_PAGES} pages)"
    )))
}

/// Every pull request in the queue other than `pr`, by position (a dequeue read after the fact, when
/// the pull request's own position is gone: all of them count as ahead).
fn all_ahead(entries: &[(i64, i64)], pr: i64) -> Vec<i64> {
    let mut v: Vec<(i64, i64)> = entries.iter().filter(|(_, n)| *n != pr).copied().collect();
    v.sort();
    v.into_iter().map(|(_, n)| n).collect()
}

/// A request of this start, run once: read, and send only when GitHub still shows the same entry
/// `UNMERGEABLE` at the request's head.
fn dequeue(program: &str, r: &Request) -> DqOutcome {
    let unread = |why: String| DqOutcome::NoRead {
        sent: false,
        ahead: None,
        why,
    };
    let read = match dq_read(program, r) {
        Ok(read) => read,
        Err(e) => return unread(e),
    };
    let not_sent = |why: String| DqOutcome::Ended {
        outcome: "not_sent",
        ahead: Vec::new(),
        state: read.raw.clone(),
        why,
    };
    if read.state != "OPEN" {
        return not_sent(format!("the pull request is {}", read.state.to_lowercase()));
    }
    if !read.head.eq_ignore_ascii_case(&r.head) {
        return not_sent(format!("the pull request's head is {}", read.head));
    }
    if !read.queued {
        return not_sent("the pull request is not in the merge queue".into());
    }
    if read.entry_id.is_some() && r.entry_id.is_some() && read.entry_id != r.entry_id {
        return not_sent("the queue entry was replaced".into());
    }
    if read.entry_state.as_deref() != Some("UNMERGEABLE") {
        return not_sent(format!(
            "the queue entry is {}",
            read.entry_state.as_deref().unwrap_or("not readable")
        ));
    }
    let entries = match queue_entries(program, r, &read.base) {
        Ok(e) => e,
        Err(QueueErr::Read(e)) => return unread(e),
        Err(QueueErr::Incomplete(why)) => {
            return DqOutcome::Ended {
                outcome: "failed",
                ahead: Vec::new(),
                state: read.raw.clone(),
                why: format!("{why}; nothing was sent"),
            };
        }
    };
    // Its own position from the same snapshot as the others (the read before is older: an entry
    // may have moved ahead since); missing from the snapshot, every entry counts as ahead.
    let own = entries.iter().find(|(_, n)| *n == r.pr).map(|(p, _)| *p);
    let ahead = match own {
        Some(p) => {
            let mut v: Vec<(i64, i64)> = entries
                .iter()
                .filter(|(q, n)| *q < p && *n != r.pr)
                .copied()
                .collect();
            v.sort();
            v.into_iter().map(|(_, n)| n).collect()
        }
        None => all_ahead(&entries, r.pr),
    };
    let answer = call(
        program,
        &[
            "api",
            "graphql",
            "--hostname",
            "github.com",
            "-f",
            &format!("query={DEQUEUE_MUTATION}"),
            "-f",
            &format!("id={}", read.node),
        ],
    );
    let (out, failure) = match answer {
        Ok(out) => (out, None),
        Err(GhError::Failed(f)) => (f.stdout.clone(), Some(f)),
        Err(e) => return after_dequeue(program, r, ahead, gh_why(&e)),
    };
    let unreadable = || match &failure {
        Some(f) => format!("{}, with no answer that can be read", f.message),
        None => "GitHub's answer could not be read".into(),
    };
    let Ok(v) = serde_json::from_str::<Value>(out.trim()) else {
        return after_dequeue(program, r, ahead, unreadable());
    };
    let errors: Vec<&Value> = v["errors"]
        .as_array()
        .map(|e| e.iter().collect())
        .unwrap_or_default();
    let data = &v["data"]["dequeuePullRequest"];
    if errors.is_empty() && data.is_object() {
        return DqOutcome::Ended {
            outcome: "dequeued",
            ahead,
            state: read.raw,
            why: String::new(),
        };
    }
    let definitive = !errors.is_empty()
        && data.is_null()
        && errors.iter().all(|e| {
            e["type"]
                .as_str()
                .is_some_and(|t| DEFINITIVE_GRAPHQL.contains(&t))
        });
    let message = errors
        .iter()
        .filter_map(|e| e["message"].as_str())
        .collect::<Vec<_>>()
        .join("; ");
    if definitive {
        return DqOutcome::Ended {
            outcome: "failed",
            ahead: Vec::new(),
            state: read.raw,
            why: format!("GitHub refused the dequeue: {message}"),
        };
    }
    after_dequeue(
        program,
        r,
        ahead,
        format!(
            "GitHub answered the dequeue with errors that do not say it was refused: {message}"
        ),
    )
}

/// A sent dequeue whose answer could not be read: read again, never sent again.
fn after_dequeue(program: &str, r: &Request, ahead: Vec<i64>, why: String) -> DqOutcome {
    match dq_read(program, r) {
        Ok(read) => {
            let out =
                read.state == "OPEN" && read.head.eq_ignore_ascii_case(&r.head) && !read.queued;
            DqOutcome::Ended {
                outcome: if out { "dequeued" } else { "unknown" },
                ahead: if out { ahead } else { Vec::new() },
                state: read.raw,
                why,
            }
        }
        Err(e) => DqOutcome::NoRead {
            sent: true,
            ahead: Some(ahead),
            why: format!("{why}; {e}"),
        },
    }
}

/// A request of an earlier start, or one this process sent and could not read: only read, and
/// every successful read ends it.
fn dequeue_read_only(program: &str, r: &Request, known: Option<Vec<i64>>) -> DqOutcome {
    let read = match dq_read(program, r) {
        Ok(read) => read,
        Err(e) => {
            return DqOutcome::NoRead {
                sent: false,
                ahead: known,
                why: e,
            };
        }
    };
    if read.state != "OPEN" || !read.head.eq_ignore_ascii_case(&r.head) {
        return DqOutcome::Ended {
            outcome: "moot",
            ahead: Vec::new(),
            state: read.raw,
            why: format!(
                "GitHub shows it {} at {}",
                read.state.to_lowercase(),
                read.head
            ),
        };
    }
    if read.queued {
        return DqOutcome::Ended {
            outcome: "unknown",
            ahead: Vec::new(),
            state: read.raw,
            why: "the dequeue may have been sent by an earlier start of makibad; GitHub shows it still queued, and it is not sent again".into(),
        };
    }
    let ahead = match known {
        Some(a) => a,
        None => match queue_entries(program, r, &read.base) {
            Ok(e) => all_ahead(&e, r.pr),
            Err(QueueErr::Read(e)) => {
                return DqOutcome::NoRead {
                    sent: false,
                    ahead: None,
                    why: e,
                };
            }
            Err(QueueErr::Incomplete(why)) => {
                return DqOutcome::Ended {
                    outcome: "unknown",
                    ahead: Vec::new(),
                    state: read.raw,
                    why: format!("out of the queue, but {why}, so what is ahead is not known"),
                };
            }
        },
    };
    DqOutcome::Ended {
        outcome: "dequeued",
        ahead,
        state: read.raw,
        why: "read after the fact".into(),
    }
}

/// The outcome of a dequeue request, on the loop's thread, in one transaction (nothing when it has
/// one already): `queue_dequeue_ended`; for `dequeued`, while the request's act or stay is still
/// the pull request's, its follow holds and its head is the run's newest, `merge_dequeued` (`why`
/// `unmergeable`) and the wait (`prwatch::unmergeable_wait`); the notice.
fn finish_dequeue(w: &StateWriter, r: &Request, o: &DqOutcome) -> Result<()> {
    let DqOutcome::Ended {
        outcome,
        ahead,
        state,
        why,
    } = o
    else {
        return Ok(());
    };
    w.transaction(|w| {
        let ended: bool = w.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM events WHERE kind = 'queue_dequeue_ended'
               AND json_extract(payload_json, '$.request') = ?1)",
            [r.id],
            |q| q.get(0),
        )?;
        if ended {
            return Ok(());
        }
        w.append_event(&NewEvent {
            kind: "queue_dequeue_ended",
            actor: Some("makibad"),
            payload: Some(&json!({"request": r.id, "repo": r.repo, "pr": r.pr, "head": r.head,
                                  "outcome": outcome, "ahead": ahead, "state": state,
                                  "why": why})),
            run_task_id: Some(&r.task),
            ..Default::default()
        })?;
        let at = format!("{}#{} at {}", r.repo, r.pr, r.head);
        match *outcome {
            "dequeued" => {
                let holds = prwatch::dequeue_key_holds(w.conn(), &r.repo, r.pr, r.key)?
                    && prwatch::followed(w.conn(), Some(&r.task))?
                        .iter()
                        .any(|f| !f.closed && f.pr == r.pr && f.repo.eq_ignore_ascii_case(&r.repo))
                    && prwatch::newest_head(w.conn(), &r.task)?
                        .is_some_and(|h| h.eq_ignore_ascii_case(&r.head));
                if holds {
                    let attempt = match r.key {
                        prwatch::DequeueKey::Act(a) => Some(a),
                        prwatch::DequeueKey::Stay(_) => None,
                    };
                    let base = state["baseRefName"].as_str().unwrap_or("its base");
                    let dequeued = door::merge_dequeued_unmergeable(
                        w, &r.task, &r.repo, r.pr, &r.head, attempt, ahead, base,
                    )?;
                    prwatch::unmergeable_wait(
                        w, &r.task, &r.repo, r.pr, &r.head, dequeued, ahead,
                        state["baseRefName"].as_str(),
                    )?;
                } else {
                    door::notice(
                        w,
                        AGENT_SECRETARY,
                        &r.task,
                        &format!(
                            "makibad took {at} out of the merge queue (its entry was UNMERGEABLE); its queue entry had already ended otherwise, so nothing more follows from makibad."
                        ),
                    )?;
                }
            }
            "failed" | "unknown" | "unreadable" => {
                door::notice(
                    w,
                    AGENT_SECRETARY,
                    &r.task,
                    &format!(
                        "makibad could not take {at} out of the merge queue (its entry is UNMERGEABLE): {why}. Take it out on GitHub; the PR watch tells you when it leaves the queue."
                    ),
                )?;
            }
            _ => {}
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

    // ---- the dequeue (Issue 575) ----------------------------------------------------------------

    use super::{DqOutcome, Request, dequeue, dequeue_read_only};
    use crate::daemon::prwatch::DequeueKey;

    /// A stand-in gh in a folder of its own: the pull request read answers the file `read` (or
    /// `read_after` once the mutation was called), the queue read answers `page1` and, with a
    /// cursor, `page2`, and the mutation answers `mutation`; every mutation call is logged in
    /// `sent`. The folder and the program's path.
    fn stand_in() -> (tempfile::TempDir, String) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().display().to_string();
        let body = format!(
            r#"#!/bin/sh
d='{d}'
cursor=
for a in "$@"; do case "$a" in cursor=*) cursor=${{a#cursor=}} ;; esac; done
case "$*" in
  *dequeuePullRequest*) echo x >> "$d/sent"; cat "$d/mutation" ;;
  *mergeQueue\(*) if [ -n "$cursor" ]; then cat "$d/page2"; else cat "$d/page1"; fi ;;
  *baseRefName*) if [ -e "$d/sent" ] && [ -e "$d/read_after" ]; then cat "$d/read_after"; else cat "$d/read"; fi ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#
        );
        let path = dir.path().join("gh");
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let program = path.display().to_string();
        (dir, program)
    }

    fn put(dir: &tempfile::TempDir, name: &str, v: &serde_json::Value) {
        std::fs::write(dir.path().join(name), v.to_string()).unwrap();
    }

    fn sent(dir: &tempfile::TempDir) -> usize {
        std::fs::read_to_string(dir.path().join("sent"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    const HEAD: &str = "1111111111111111111111111111111111111111";

    fn request(boot: &str) -> Request {
        Request {
            id: 7,
            repo: "o/r".into(),
            pr: 1,
            head: HEAD.into(),
            task: "t".into(),
            key: DequeueKey::Act(3),
            entry_id: Some("MQE_1".into()),
            boot: boot.into(),
        }
    }

    fn pr(state: &str, queued: bool, position: i64) -> serde_json::Value {
        let entry = if queued {
            serde_json::json!({"id": "MQE_1", "state": "UNMERGEABLE", "position": position})
        } else {
            serde_json::Value::Null
        };
        serde_json::json!({"data": {"repository": {"pullRequest": {
            "id": "PR_1", "state": state, "headRefOid": HEAD, "baseRefName": "main",
            "isInMergeQueue": queued, "mergeQueueEntry": entry}}}})
    }

    fn page(entries: &[(i64, i64)], next: Option<&str>) -> serde_json::Value {
        let nodes: Vec<serde_json::Value> = entries
            .iter()
            .map(|(p, n)| serde_json::json!({"position": p, "pullRequest": {"number": n}}))
            .collect();
        serde_json::json!({"data": {"repository": {"mergeQueue": {"entries": {
            "pageInfo": {"hasNextPage": next.is_some(), "endCursor": next},
            "nodes": nodes}}}}})
    }

    fn ended(o: &DqOutcome) -> (&'static str, Vec<i64>) {
        match o {
            DqOutcome::Ended { outcome, ahead, .. } => (outcome, ahead.clone()),
            DqOutcome::NoRead { .. } => panic!("not ended: {o:?}"),
        }
    }

    /// A request of an earlier start is only read: still queued is `unknown` and nothing is sent;
    /// out of the queue at its head is `dequeued` with every entry now queued ahead; merged is
    /// `moot`.
    #[test]
    fn a_request_of_an_earlier_start_is_read_and_never_sent() {
        let (dir, gh) = stand_in();
        put(&dir, "page1", &page(&[(1, 2), (2, 4)], None));
        put(&dir, "read", &pr("OPEN", true, 3));
        let r = request("an earlier start");
        assert_eq!(ended(&dequeue_read_only(&gh, &r, None)).0, "unknown");
        assert_eq!(sent(&dir), 0, "an earlier start's request is never sent");
        put(&dir, "read", &pr("OPEN", false, 0));
        assert_eq!(
            ended(&dequeue_read_only(&gh, &r, None)),
            ("dequeued", vec![2, 4])
        );
        put(&dir, "read", &pr("MERGED", false, 0));
        assert_eq!(ended(&dequeue_read_only(&gh, &r, None)).0, "moot");
        assert_eq!(sent(&dir), 0);
    }

    /// A sent dequeue whose answer cannot be read is read again and never sent again: out of the
    /// queue is `dequeued`, still queued is `unknown`; the queue is read page after page for the
    /// entries ahead.
    #[test]
    fn a_sent_dequeue_whose_answer_cannot_be_read_is_read_not_sent_again() {
        let (dir, gh) = stand_in();
        put(&dir, "page1", &page(&[(1, 2)], Some("c1")));
        put(&dir, "page2", &page(&[(2, 4), (3, 1), (4, 5)], None));
        put(&dir, "read", &pr("OPEN", true, 3));
        std::fs::write(dir.path().join("mutation"), "not json").unwrap();
        put(&dir, "read_after", &pr("OPEN", false, 0));
        let r = request(super::boot());
        assert_eq!(ended(&dequeue(&gh, &r)), ("dequeued", vec![2, 4]));
        assert_eq!(sent(&dir), 1);

        // The entries ahead come from the queue's own snapshot, not the earlier read's position:
        // PR 5 moved ahead of it between the two reads.
        let (dir, gh) = stand_in();
        put(&dir, "page1", &page(&[(1, 2), (2, 5), (3, 1)], None));
        put(&dir, "read", &pr("OPEN", true, 2));
        put(
            &dir,
            "mutation",
            &serde_json::json!({"data": {"dequeuePullRequest": {"mergeQueueEntry": {"id": "MQE_1"}}}}),
        );
        assert_eq!(ended(&dequeue(&gh, &r)), ("dequeued", vec![2, 5]));
        let (dir, gh) = stand_in();
        put(&dir, "page1", &page(&[(1, 2), (2, 4)], None));
        put(&dir, "read", &pr("OPEN", true, 1));
        put(
            &dir,
            "mutation",
            &serde_json::json!({"data": {"dequeuePullRequest": {"mergeQueueEntry": {"id": "MQE_1"}}}}),
        );
        assert_eq!(
            ended(&dequeue(&gh, &r)),
            ("dequeued", vec![2, 4]),
            "missing from the snapshot: every entry is ahead"
        );

        let (dir, gh) = stand_in();
        put(&dir, "page1", &page(&[(1, 2), (2, 1)], None));
        put(&dir, "read", &pr("OPEN", true, 2));
        std::fs::write(dir.path().join("mutation"), "not json").unwrap();
        assert_eq!(ended(&dequeue(&gh, &r)).0, "unknown");
        assert_eq!(sent(&dir), 1, "sent once");
    }

    /// A queue longer than makibad reads is `failed` and nothing is sent; a read that shows
    /// another entry, or no longer `UNMERGEABLE`, is `not_sent`.
    #[test]
    fn a_queue_too_long_to_read_or_a_changed_entry_sends_nothing() {
        let (dir, gh) = stand_in();
        put(&dir, "page1", &page(&[(1, 2)], Some("c1")));
        put(&dir, "page2", &page(&[(2, 4)], Some("c2")));
        put(&dir, "read", &pr("OPEN", true, 9));
        let r = request(super::boot());
        assert_eq!(ended(&dequeue(&gh, &r)).0, "failed");
        assert_eq!(sent(&dir), 0, "an incomplete queue sends nothing");

        let mut other = pr("OPEN", true, 2);
        other["data"]["repository"]["pullRequest"]["mergeQueueEntry"]["id"] =
            serde_json::json!("MQE_9");
        put(&dir, "read", &other);
        assert_eq!(ended(&dequeue(&gh, &r)).0, "not_sent");
        let mut fine = pr("OPEN", true, 2);
        fine["data"]["repository"]["pullRequest"]["mergeQueueEntry"]["state"] =
            serde_json::json!("AWAITING_CHECKS");
        put(&dir, "read", &fine);
        assert_eq!(ended(&dequeue(&gh, &r)).0, "not_sent");
        assert_eq!(sent(&dir), 0);
    }

    /// Diff review round 1: an entry of the queue without a readable position or pull request
    /// (GitHub's fields are nullable) is never skipped: before sending it is `failed` and nothing
    /// is sent; an earlier start's request read out of the queue is `unknown`, never `dequeued`
    /// with an entry ahead missing.
    #[test]
    fn a_queue_entry_without_a_position_or_pull_request_sends_nothing() {
        let blank = |node: serde_json::Value| {
            serde_json::json!({"data": {"repository": {"mergeQueue": {"entries": {
                "pageInfo": {"hasNextPage": false, "endCursor": null},
                "nodes": [{"position": 1, "pullRequest": {"number": 2}}, node,
                          {"position": 3, "pullRequest": {"number": 1}}]}}}}})
        };
        let r = request(super::boot());
        for node in [
            serde_json::json!({"position": 2, "pullRequest": null}),
            serde_json::json!({"position": null, "pullRequest": {"number": 4}}),
            serde_json::Value::Null,
        ] {
            let (dir, gh) = stand_in();
            put(&dir, "page1", &blank(node.clone()));
            put(&dir, "read", &pr("OPEN", true, 3));
            let o = dequeue(&gh, &r);
            assert_eq!(ended(&o).0, "failed", "{node}: {o:?}");
            assert_eq!(sent(&dir), 0, "{node}: nothing sent");
            put(&dir, "read", &pr("OPEN", false, 0));
            let o = dequeue_read_only(&gh, &request("an earlier start"), None);
            assert_eq!(ended(&o).0, "unknown", "{node}: {o:?}");
            assert_eq!(sent(&dir), 0);
        }
    }
}
