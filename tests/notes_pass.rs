//! Issue 270: the pass over claude-org-ja's notes, and the rules of its knowledge-curation
//! contract that makiba's curator rules lacked (a to e).
//!
//! - The verb `candidate` takes `deferred` (rule b): a held entry is not terminal and is decided
//!   later. A pattern that returns after a decision has a new open entry beside the decided one
//!   (rule c), and the verb writes the open one.
//! - The rules of the worker, the curator and the secretary carry rule e (nothing
//!   operator-private in a tracked file); the curator's carry rule a; the curate skill names
//!   `deferred` and the count of rule d.
//! - The record of the pass exists and every carried note names where it went.
//! - Incident I-12: every entry of the sandbox's `denyRead` and `denyWrite` in a role's settings
//!   is a string.
//!
//! The verb is called on the real binary, `makiba daemon` on the stand-in base with
//! `MAKIBA_DOOR=none`, over the MCP port, as `tests/candidate_verb.rs` does.

use makiba::db::writer::StateWriter;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

const SECRETARY_KEY: &str = "5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5";

fn entry(pattern: &str, date: &str, status: &str, decided: &str, reason: &str) -> String {
    let sp = |v: &str| {
        if v.is_empty() {
            String::new()
        } else {
            format!(" {v}")
        }
    };
    format!(
        "- pattern: {pattern}\n- date: {date}\n- notes: raw/{date}-{pattern}.md\n\
         - when: A test needs {pattern}.\n- skill: {pattern}\n- status: {status}\n\
         - decided:{}\n- reason:{}\n\n",
        sp(decided),
        sp(reason)
    )
}

/// The file as the curator left it.
fn before() -> String {
    [
        "# Skill candidates\n\n".to_string(),
        // Held, then approved.
        entry("hold-then-approve", "2026-11-19", "pending", "", ""),
        // Held, then rejected; held, then folded into a skill.
        entry("hold-then-reject", "2026-11-19", "pending", "", ""),
        entry("hold-then-fold", "2026-11-19", "pending", "", ""),
        // A pattern that returned after a decision: the decided entry and a new open one.
        entry(
            "came-back",
            "2026-10-01",
            "rejected",
            "2026-10-02",
            "not now",
        ),
        entry("came-back", "2026-11-19", "pending", "", ""),
        // A pattern decided once, held once.
        entry(
            "back-and-held",
            "2026-10-01",
            "rejected",
            "2026-10-02",
            "no",
        ),
        entry(
            "back-and-held",
            "2026-11-10",
            "deferred",
            "2026-11-11",
            "later",
        ),
        // Two open entries of one pattern: the curator's to merge.
        entry(
            "twice-open",
            "2026-11-18",
            "deferred",
            "2026-11-18",
            "later",
        ),
        entry("twice-open", "2026-11-19", "pending", "", ""),
        // An entry of the pattern with no status line, beside an open one.
        "- pattern: no-status\n- date: 2026-11-01\n- notes: raw/x.md\n- when: W.\n\
         - skill: no-status\n- decided:\n- reason:\n\n"
            .to_string(),
        entry("no-status", "2026-11-19", "pending", "", ""),
        // Decided only.
        entry("all-decided", "2026-10-01", "approved", "2026-10-02", "yes"),
    ]
    .concat()
}

fn sha256(text: &str) -> String {
    let mut child = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()[..64].to_string()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A `makibad` that is killed when the test ends, so a failed test leaves nothing running.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// One tool call on the port with a bearer token: whether it is a tool error, and the JSON of its
/// text.
fn call(addr: &str, token: &str, tool: &str, args: Value) -> (bool, Value) {
    let body = json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": {"name": tool, "arguments": args}})
    .to_string();
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut all = String::new();
    s.read_to_string(&mut all).unwrap();
    let body = all.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    let r: Value = serde_json::from_str(body).unwrap_or_else(|e| panic!("not JSON: {all:?}: {e}"));
    let result = &r["result"];
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no tool result: {r}"));
    (
        result["isError"] == json!(true),
        serde_json::from_str(text).unwrap(),
    )
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && [0, 1, 2, 3, 5, 6, 8, 9]
            .iter()
            .all(|i| b[*i].is_ascii_digit())
        && b[4] == b'-'
        && b[7] == b'-'
}

/// A `makibad` over a knowledge folder holding `before()`: its address, the candidates file and
/// the database.
fn start(root: &Path) -> (Daemon, String, PathBuf, PathBuf) {
    for d in ["home", "state", "config", "data", "run", "xdg-state"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    let knowledge = root.join("knowledge");
    std::fs::create_dir_all(&knowledge).unwrap();
    let file = knowledge.join("skill-candidates.md");
    std::fs::write(&file, before()).unwrap();
    let db = root.join("state/state.db");
    let w = StateWriter::open(&db).unwrap();
    w.conn()
        .execute(
            "INSERT INTO agents (agent_id, role, run_id, key_sha256)
             VALUES ('secretary', 'secretary', NULL, ?1)",
            [sha256(SECRETARY_KEY)],
        )
        .unwrap();
    drop(w);
    let port = free_port();
    let daemon = Daemon(
        Command::new(MAKIBA)
            .env("HOME", root.join("home"))
            .env("MAKIBA_STATE_DIR", root.join("state"))
            .env("XDG_STATE_HOME", root.join("xdg-state"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_RUNTIME_DIR", root.join("run"))
            .env("MAKIBA_KNOWLEDGE_DIR", &knowledge)
            .env("MAKIBA_DOOR", "none")
            .env("MAKIBA_PORT", port.to_string())
            .env("MAKIBA_BASE", "stub")
            .env("MAKIBA_STUB_SUBSCRIBE_DELAY_MS", "0")
            .env_remove("MAKIBA_PERSON_KEY")
            .arg("daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let addr = format!("127.0.0.1:{port}");
    let until = Instant::now() + Duration::from_secs(15);
    while Instant::now() < until && TcpStream::connect(&addr).is_err() {
        std::thread::sleep(Duration::from_millis(50));
    }
    (daemon, addr, file, db)
}

#[test]
fn a_held_candidate_is_decided_later_and_a_returning_pattern_is_decided_in_its_open_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let (_daemon, addr, file, db) = start(tmp.path());
    // The decisions recorded for a pattern: the status written, in order.
    let events = |pattern: &str| -> Vec<String> {
        let c = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut q = c
            .prepare(
                "SELECT json_extract(payload_json, '$.status') FROM events
                  WHERE kind = 'skill_candidate_decided'
                    AND json_extract(payload_json, '$.pattern') = ?1 ORDER BY id",
            )
            .unwrap();
        q.query_map([pattern], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let mut n = 0;
    let mut decide = |pattern: &str, decision: &str, reason: &str| {
        n += 1;
        call(
            &addr,
            SECRETARY_KEY,
            "candidate",
            json!({"pattern": pattern, "decision": decision, "reason": reason,
                   "idem": format!("i{n}")}),
        )
    };
    let read = || std::fs::read_to_string(&file).unwrap();
    let open = "- status: pending\n- decided:\n- reason:\n";
    let mut want = before();

    // 1. `deferred` on a pending entry writes the status, the date and the reason, and the event.
    let mut date = String::new();
    for pattern in ["hold-then-approve", "hold-then-reject", "hold-then-fold"] {
        let (is_error, held) = decide(pattern, "deferred", "after the release");
        assert!(!is_error, "{held}");
        assert_eq!(held["status"], json!("deferred"), "{held}");
        date = held["decided"].as_str().unwrap_or_default().to_string();
        assert!(is_date(&date), "{held}");
        let old = format!("- skill: {pattern}\n{open}");
        assert!(want.contains(&old));
        want = want.replacen(
            &old,
            &format!(
                "- skill: {pattern}\n- status: deferred\n- decided: {date}\n\
                 - reason: after the release\n"
            ),
            1,
        );
        assert_eq!(read(), want, "after deferring {pattern}");
        assert_eq!(events(pattern), ["deferred"]);
    }

    // 2. A held entry is not held twice: `conflict`, the file and the events as they were.
    let (is_error, again) = decide("hold-then-approve", "deferred", "still later");
    assert!(is_error, "{again}");
    assert_eq!(again["kind"], json!("conflict"), "{again}");
    assert_eq!(read(), want);
    assert_eq!(events("hold-then-approve"), ["deferred"]);

    // 3. A held entry is decided later, by each of the three decisions.
    for (pattern, decision, status) in [
        ("hold-then-approve", "approved", "approved"),
        ("hold-then-reject", "rejected", "rejected"),
        (
            "hold-then-fold",
            "into-other-skill",
            "merged-into-other-skill",
        ),
    ] {
        let (is_error, done) = decide(pattern, decision, "the person looked again");
        assert!(!is_error, "{done}");
        assert_eq!(done["status"], json!(status), "{done}");
        let old = format!(
            "- skill: {pattern}\n- status: deferred\n- decided: {date}\n\
             - reason: after the release\n"
        );
        assert!(want.contains(&old));
        want = want.replacen(
            &old,
            &format!(
                "- skill: {pattern}\n- status: {status}\n- decided: {date}\n\
                 - reason: the person looked again\n"
            ),
            1,
        );
        assert_eq!(read(), want, "after {decision} on {pattern}");
        assert_eq!(events(pattern), ["deferred", status]);
    }

    // 4. A pattern that returned: the decision goes into its open entry, and the decided entry
    //    keeps every byte.
    let (is_error, back) = decide("came-back", "approved", "it keeps coming");
    assert!(!is_error, "{back}");
    let old = format!(
        "- date: 2026-11-19\n- notes: raw/2026-11-19-came-back.md\n- when: A test needs came-back.\n- skill: came-back\n{open}"
    );
    assert!(want.contains(&old));
    want = want.replacen(
        &old,
        &old.replace(
            open,
            &format!("- status: approved\n- decided: {date}\n- reason: it keeps coming\n"),
        ),
        1,
    );
    assert_eq!(read(), want);
    assert!(read().contains("- status: rejected\n- decided: 2026-10-02\n- reason: not now\n"));
    assert_eq!(events("came-back"), ["approved"]);

    // 5. The same with a held entry beside the decided one.
    let (is_error, held) = decide("back-and-held", "rejected", "no again");
    assert!(!is_error, "{held}");
    let old = "- status: deferred\n- decided: 2026-11-11\n- reason: later\n";
    assert!(want.contains(old));
    want = want.replacen(
        old,
        &format!("- status: rejected\n- decided: {date}\n- reason: no again\n"),
        1,
    );
    assert_eq!(read(), want);
    assert_eq!(events("back-and-held"), ["rejected"]);

    // 6. Refused, with the file and the events unchanged: two open entries of one pattern, an
    //    entry of the pattern without a status line, a pattern with no open entry.
    for pattern in ["twice-open", "no-status", "all-decided", "came-back"] {
        let had = events(pattern);
        for decision in ["approved", "deferred"] {
            let (is_error, refused) = decide(pattern, decision, "r");
            assert!(is_error, "{pattern} {decision}: {refused}");
            assert_eq!(refused["kind"], json!("conflict"), "{pattern}: {refused}");
            assert_eq!(read(), want, "{pattern} {decision} changed the file");
            assert_eq!(events(pattern), had);
        }
    }
}

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn text(rel: &str) -> String {
    std::fs::read_to_string(repo(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

#[test]
fn the_rules_carry_the_curation_rules_the_contract_had() {
    // e: nothing operator-private in a tracked file, in the rules of all three roles.
    for role in ["worker", "curator", "secretary"] {
        let rules = text(&format!("roles/{role}/rules.md"));
        assert!(
            rules.contains("operator-private"),
            "roles/{role}/rules.md lacks the rule on operator-private content"
        );
    }
    let worker = text("roles/worker/rules.md");
    assert!(
        worker.contains("commit message"),
        "the worker's rule does not name the commit message"
    );
    let curator = text("roles/curator/rules.md");
    // a: the edit policy and its one invariant.
    assert!(
        curator.contains("silently"),
        "the curator's rules lack the edit policy's invariant"
    );
    // b and c: the held entry, and the pattern that returns after a decision.
    assert!(
        curator.contains("`deferred`"),
        "the curator's rules do not name deferred"
    );
    assert!(
        curator.contains("most recent"),
        "the curator's rules do not say which decision a returning pattern is counted from"
    );
    let skill = text("skills/curate/SKILL.md");
    assert!(
        skill.contains("`deferred`"),
        "skills/curate does not name deferred"
    );
    // d: the count for asking.
    assert!(
        skill.contains("5 or more are `pending`"),
        "skills/curate has no count for asking"
    );
    assert!(
        !skill.contains("whatever its status, is not proposed again"),
        "skills/curate still says a decided pattern never returns"
    );
}

/// The cells of the record's table rows: note, verdict, what, where.
fn record_rows(record: &str) -> Vec<Vec<String>> {
    let (_, table) = record
        .split_once("\n## Every note\n")
        .expect("the record has no section `Every note`");
    let table = table.split("\n## ").next().unwrap_or_default();
    table
        .lines()
        .filter(|l| l.starts_with("| `"))
        .map(|l| {
            l.trim()
                .trim_matches('|')
                .split('|')
                .map(|c| c.trim().to_string())
                .collect()
        })
        .collect()
}

fn backticked(cell: &str) -> Vec<&str> {
    cell.split('`').skip(1).step_by(2).collect()
}

#[test]
fn the_record_of_the_pass_says_where_every_carried_note_went() {
    let record = text("docs/design/notes-pass.md");
    let rows = record_rows(&record);
    assert!(!rows.is_empty(), "the record has no table row");
    let (mut dropped, mut carried) = (0, 0);
    for row in &rows {
        assert_eq!(row.len(), 4, "a row of four cells: {row:?}");
        match row[1].as_str() {
            "drop" => dropped += 1,
            "carry" => {
                carried += 1;
                let places = backticked(&row[3]);
                assert!(
                    !places.is_empty(),
                    "{} is carried to no named place",
                    row[0]
                );
                for place in places {
                    // A file of this repository, or a raw note of the knowledge folder.
                    assert!(
                        place.starts_with("raw/") || repo(place).exists(),
                        "{}: {place} is neither a file here nor a raw note",
                        row[0]
                    );
                }
            }
            v => panic!("{}: the verdict is drop or carry, not {v:?}", row[0]),
        }
        assert!(!row[2].is_empty(), "{} gives no reason", row[0]);
    }
    assert!(carried > 0 && dropped > 0);
    let counts = format!(
        "Notes read: {}. Dropped: {dropped}. Carried: {carried}.",
        rows.len()
    );
    assert!(
        record.contains(&counts),
        "the record's counts are not those of its table ({counts})"
    );
    // The incident the pass carried is in the table of incidents, guarded by this file.
    let incidents = text("docs/design/incidents.md");
    let row = incidents
        .lines()
        .find(|l| l.starts_with("| I-12 "))
        .expect("docs/design/incidents.md has no row I-12");
    assert!(row.contains("`tests/notes_pass.rs`"), "{row}");
}

/// Incident I-12: one entry that is not a string in the sandbox's `denyRead` or `denyWrite` made
/// Claude Code apply neither the permission rules nor the hooks, with no warning.
#[test]
fn every_sandbox_deny_entry_of_a_role_is_a_string() {
    for role in ["secretary", "worker", "curator"] {
        let rel = format!("roles/{role}/settings.json");
        let v: Value = serde_json::from_str(&text(&rel)).unwrap();
        for list in ["denyRead", "denyWrite"] {
            let entries = v["sandbox"]["filesystem"][list]
                .as_array()
                .unwrap_or_else(|| panic!("{rel}: sandbox.filesystem.{list} is not an array"));
            assert!(!entries.is_empty(), "{rel}: {list} is empty");
            for e in entries {
                assert!(e.is_string(), "{rel}: {list} holds {e}, not a string");
            }
        }
    }
}
