//! Issue 368: the test agent's record file is written by several processes at once (an agent,
//! its detached grandchildren, other agents on the same record). Every row must stay one line:
//! a row and its newline written apart let another process's row land between them, and two
//! rows meet on one line. These tests run only the test agent, many at once on one record.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const FAKE: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");
const AGENTS: usize = 32;
const VARS: usize = 50;

fn lines(record: &Path) -> Vec<String> {
    std::fs::read_to_string(record)
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

fn counts(rows: &[String]) -> BTreeMap<&str, usize> {
    let mut m = BTreeMap::new();
    for r in rows {
        *m.entry(r.as_str()).or_insert(0) += 1;
    }
    m
}

#[test]
fn rows_of_agents_writing_at_once_never_share_a_line() {
    let dir = tempfile::tempdir().unwrap();
    for round in 0..5 {
        let record = dir.path().join(format!("record-{round}"));
        let rec = record.display().to_string();
        let children: Vec<Child> = (0..AGENTS)
            .map(|_| {
                let mut cmd = Command::new(FAKE);
                cmd.args(["--record", &rec, "env"])
                    .env_clear()
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                for i in 0..VARS {
                    cmd.env(format!("ROW_{i:02}"), "v");
                }
                cmd.spawn().unwrap()
            })
            .collect();
        for mut c in children {
            assert!(
                c.wait().unwrap().success(),
                "round {round}: an agent failed"
            );
        }

        let rows = lines(&record);
        let empty = rows.iter().filter(|r| r.is_empty()).count();
        assert_eq!(
            empty, 0,
            "round {round}: {empty} empty lines (rows met on one line)"
        );
        assert_eq!(rows.len(), AGENTS * (VARS + 5), "round {round}: line count");

        let mut want: Vec<String> = (0..VARS).map(|i| format!("env ROW_{i:02}=v")).collect();
        want.push(format!("arg {FAKE}"));
        want.push("arg --record".into());
        want.push(format!("arg {rec}"));
        want.push("arg env".into());
        want.push("env-end".into());
        let got = counts(&rows);
        for r in &rows {
            assert!(want.contains(r), "round {round}: unexpected line {r:?}");
        }
        for w in &want {
            assert_eq!(got.get(w.as_str()), Some(&AGENTS), "round {round}: {w:?}");
        }
    }
}

#[test]
fn detached_rows_written_at_once_never_share_a_line() {
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("record");
    let rec = record.display().to_string();
    let sock = dir.path().join("absent.sock").display().to_string();
    let verbs = ["a", "b", "c", "d"];
    let children: Vec<Child> = (0..AGENTS)
        .map(|_| {
            let mut cmd = Command::new(FAKE);
            cmd.args(["--record", &rec]);
            for v in verbs {
                cmd.arg(format!("detach:door:{v}:{sock}"));
            }
            cmd.stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for mut c in children {
        assert!(c.wait().unwrap().success(), "an agent failed");
    }

    let want = AGENTS * verbs.len();
    let until = Instant::now() + Duration::from_secs(30);
    let mut rows = lines(&record);
    while rows.len() < want && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(100));
        rows = lines(&record);
    }
    // The grandchildren that are still writing have had their time: read once more.
    std::thread::sleep(Duration::from_millis(200));
    rows = lines(&record);

    let empty = rows.iter().filter(|r| r.is_empty()).count();
    assert_eq!(
        empty, 0,
        "{empty} empty lines (rows met on one line): {rows:#?}"
    );
    assert_eq!(rows.len(), want, "the record: {rows:#?}");
    let mut per_verb: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &rows {
        assert!(r.starts_with("detached ppid="), "{r}");
        assert_eq!(r.matches("detached ").count(), 1, "{r}");
        let fields: Vec<&str> = r.split(' ').collect();
        assert_eq!(fields.get(6), Some(&"door"), "{r}");
        let verb = fields.get(7).copied().unwrap_or_default();
        assert!(verbs.contains(&verb), "{r}");
        *per_verb.entry(verb).or_insert(0) += 1;
    }
    for v in verbs {
        assert_eq!(per_verb.get(v), Some(&AGENTS), "{v}: {per_verb:?}");
    }
}
