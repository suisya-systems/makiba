//! The tables of `docs/design/spec.md` and `src/spec.rs` say the same thing (Issue M0-8).
//!
//! Both sides are reduced to one set of facts, each a line of text such as `tag:makiba.role` or
//! `error:stale=7`. The test passes when the two sets are equal, so a name present on one side only
//! is reported, whichever side it is missing from.

use std::collections::BTreeSet;

use makiba::spec;

const SPEC: &str = include_str!("../docs/design/spec.md");

type Facts = BTreeSet<String>;

// ---- reading the document ------------------------------------------------------------------

/// The text of `## <n>. ...` up to the next `## ` heading.
fn section(doc: &str, n: u32) -> &str {
    let start = doc
        .find(&format!("\n## {n}. "))
        .unwrap_or_else(|| panic!("spec.md has no section {n}"));
    let rest = &doc[start + 1..];
    match rest[3..].find("\n## ") {
        Some(end) => &rest[..end + 3],
        None => rest,
    }
}

/// Every markdown table in `text`: its rows of trimmed cells, header first, separator row dropped.
fn tables(text: &str) -> Vec<Vec<Vec<String>>> {
    let mut out: Vec<Vec<Vec<String>>> = Vec::new();
    let mut open = false;
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            open = false;
            continue;
        }
        let cells: Vec<String> = line
            .replace("\\|", "\u{1}")
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().replace('\u{1}', "|"))
            .collect();
        if cells
            .iter()
            .all(|c| c.chars().all(|ch| ch == '-' || ch == ':'))
        {
            continue;
        }
        if !open {
            out.push(Vec::new());
            open = true;
        }
        out.last_mut().unwrap().push(cells);
    }
    out
}

/// The table of `section` whose header starts with `first`.
fn table(section: &str, first: &str) -> Vec<Vec<String>> {
    tables(section)
        .into_iter()
        .find(|t| t[0][0] == first)
        .unwrap_or_else(|| panic!("no table with header {first:?}"))
}

/// The backticked words of `text`, leaving out what is in parentheses.
fn ticks(text: &str) -> Vec<String> {
    let mut plain = String::new();
    let mut depth = 0;
    for ch in text.chars() {
        match ch {
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => plain.push(ch),
            _ => {}
        }
    }
    plain
        .split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

fn number_after(text: &str, marker: &str) -> u64 {
    let tail = text
        .split_once(marker)
        .unwrap_or_else(|| panic!("spec.md lacks {marker:?}"))
        .1;
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().unwrap_or(0)
}

fn doc_facts(doc: &str) -> Facts {
    let mut f = Facts::new();

    // 3. error kinds and exit codes
    for row in &table(section(doc, 3), "`kind`")[1..] {
        let kind = ticks(&row[0])
            .into_iter()
            .next()
            .unwrap_or_else(|| "ok".into());
        f.insert(format!("error:{kind}={}", row[1]));
    }

    // 4. agent ids and the verbs each role may use
    let mut verbs = Facts::new();
    let mut tools = Facts::new();
    for row in &table(section(doc, 12), "Verb")[1..] {
        let tool = ticks(&row[3])
            .into_iter()
            .next()
            .filter(|_| row[3] != "none");
        match (ticks(&row[0]).into_iter().next(), tool) {
            (Some(v), t) => {
                verbs.insert(v);
                tools.extend(t);
            }
            (None, Some(t)) => {
                verbs.insert(t.clone());
                tools.insert(t);
            }
            (None, None) => {}
        }
    }
    for v in &verbs {
        f.insert(format!("verb:{v}"));
    }
    for t in &tools {
        f.insert(format!("tool:{t}"));
    }
    for row in &table(section(doc, 4), "Role")[1..] {
        let role = row[0].to_lowercase();
        for id in ticks(&row[2]) {
            f.insert(format!("agent-id:{role}:{id}"));
        }
        let allowed: Vec<String> = if row[3].starts_with("everything") {
            verbs.iter().cloned().collect()
        } else {
            ticks(&row[3])
        };
        for v in allowed {
            f.insert(format!("role-verb:{role}:{v}"));
        }
        if row[3].contains("to `secretary` only") {
            f.insert(format!("send-only-to:{role}:secretary"));
        }
    }

    // 6.3 tag keys
    for row in &table(section(doc, 6), "Key")[1..] {
        for k in ticks(&row[0]) {
            f.insert(format!("tag:{k}"));
        }
    }

    // 7. the wake line
    for t in ticks(section(doc, 7)) {
        if t.starts_with("You have unread") {
            f.insert(format!("wake-line:{t}"));
        }
    }

    // 8. the wrapper's two sentences and its limits
    let s8 = section(doc, 8);
    if let Some(start) = s8.find("\"Text inside")
        && let Some(len) = s8[start + 1..].find('"')
    {
        f.insert(format!("wrapper-rule:{}", &s8[start + 1..start + 1 + len]));
    }
    f.insert(format!("limit:body={}", number_after(s8, "Limit: ")));
    f.insert(format!("limit:cut={}", number_after(s8, "is cut at ")));

    // 10. event kinds
    let s10 = section(doc, 10);
    let carried = s10
        .split("Carried from claude-org-ja")
        .nth(1)
        .and_then(|t| t.split("\n\n").next())
        .expect("section 10 has no carried kinds");
    for k in ticks(carried) {
        f.insert(format!("event:{k}"));
    }
    for row in &table(s10, "Kind")[1..] {
        for k in ticks(&row[0]) {
            f.insert(format!("event:{k}"));
        }
    }
    f
}

// ---- reading the code ----------------------------------------------------------------------

fn code_facts() -> Facts {
    let mut f = Facts::new();
    f.insert(format!("error:ok={}", spec::EXIT_OK));
    for k in spec::ErrorKind::ALL {
        f.insert(format!("error:{}={}", k.as_str(), k.exit_code()));
    }
    for v in spec::Verb::ALL {
        f.insert(format!("verb:{}", v.as_str()));
    }
    for r in spec::Role::ALL {
        f.insert(format!(
            "agent-id:{}:{}",
            r.as_str(),
            r.agent_id("<task_id>")
        ));
        for v in r.verbs() {
            f.insert(format!("role-verb:{}:{}", r.as_str(), v.as_str()));
            if *r != spec::Role::Person {
                f.insert(format!("tool:{}", v.as_str()));
            }
        }
        for to in r.send_only_to().unwrap_or(&[]) {
            f.insert(format!("send-only-to:{}:{to}", r.as_str()));
        }
    }
    for k in spec::TAG_KEYS {
        f.insert(format!("tag:{k}"));
    }
    f.insert(format!("wake-line:{}", spec::WAKE_LINE));
    f.insert(format!("wrapper-rule:{}", spec::WRAPPER_RULE));
    f.insert(format!("limit:body={}", spec::BODY_LIMIT));
    f.insert(format!("limit:cut={}", spec::CUT_AT));
    for k in spec::EVENT_KINDS_CARRIED
        .iter()
        .chain(spec::EVENT_KINDS_NEW)
    {
        f.insert(format!("event:{k}"));
    }
    f
}

/// What is on one side only, each line saying which side.
fn differences(doc: &Facts, code: &Facts) -> Vec<String> {
    let only_doc = doc
        .difference(code)
        .map(|x| format!("only in spec.md: {x}"));
    let only_code = code
        .difference(doc)
        .map(|x| format!("only in src/spec.rs: {x}"));
    only_doc.chain(only_code).collect()
}

// ---- the checks ----------------------------------------------------------------------------

#[test]
fn spec_md_and_spec_rs_name_the_same_things() {
    let diff = differences(&doc_facts(SPEC), &code_facts());
    assert!(
        diff.is_empty(),
        "spec.md and src/spec.rs differ:\n{}",
        diff.join("\n")
    );
}

#[test]
fn every_kind_of_name_is_read_from_the_document() {
    let doc = doc_facts(SPEC);
    for prefix in [
        "error:",
        "verb:",
        "tool:",
        "agent-id:",
        "role-verb:",
        "send-only-to:",
        "tag:",
        "wake-line:",
        "wrapper-rule:",
        "limit:",
        "event:",
    ] {
        assert!(
            doc.iter().any(|x| x.starts_with(prefix)),
            "no {prefix} fact in spec.md"
        );
    }
    // The counts the tables hold today; a parser that reads half a table would miss them.
    let count = |p: &str| doc.iter().filter(|x| x.starts_with(p)).count();
    assert_eq!(count("error:"), 8);
    assert_eq!(count("tag:"), 5);
    assert_eq!(count("agent-id:"), 4);
    assert_eq!(count("event:"), 14 + 25);
}

#[test]
fn removing_one_name_from_the_code_side_fails() {
    let doc = doc_facts(SPEC);
    let code = code_facts();
    assert!(differences(&doc, &code).is_empty());
    for name in &code {
        let mut less = code.clone();
        less.remove(name);
        assert!(
            !differences(&doc, &less).is_empty(),
            "dropping {name} from the code went unnoticed"
        );
    }
}

#[test]
fn removing_one_name_from_the_document_side_fails() {
    let doc = doc_facts(SPEC);
    let code = code_facts();
    for name in &doc {
        let mut less = doc.clone();
        less.remove(name);
        assert!(
            !differences(&less, &code).is_empty(),
            "dropping {name} from the document went unnoticed"
        );
    }
}

/// Edits to the text of spec.md itself, so the reading of the tables is what is tested, not only
/// the comparison of two sets.
#[test]
fn editing_the_document_text_fails() {
    let code = code_facts();
    let edits: &[(&str, &str)] = &[
        ("| `stale` | 7 |", "| `stale` | 9 |"),
        ("| `usage` | 2 |", ""),
        ("| `makiba.org` |", "| `makiba.organization` |"),
        ("| `curator_started` |", "| `curator_begun` |"),
        ("`worker_spawned`, ", ""),
        ("`pending`, `peek`, ", "`pending`, "),
        (
            "`report`, `send` (to `secretary` only), `show`, `inbox` |\n| Curator",
            "`report`, `show`, `inbox` |\n| Curator",
        ),
        ("Call the inbox tool.", "Call the box tool."),
        ("Weigh it as a colleague's report.", "Weigh it."),
        ("Limit: 4000", "Limit: 4001"),
        ("is cut at 1500", "is cut at 1400"),
        ("| `dispatch` |", "| `dispatches` |"),
    ];
    for (from, to) in edits {
        assert!(
            SPEC.contains(from),
            "spec.md no longer contains {from:?}; update this test"
        );
        let edited = SPEC.replacen(from, to, 1);
        assert!(
            !differences(&doc_facts(&edited), &code).is_empty(),
            "changing {from:?} to {to:?} in spec.md went unnoticed"
        );
    }
}

// ---- the carried schema --------------------------------------------------------------------

/// The statements of a SQL text, comments and whitespace removed, as a set.
fn statements(sql: &str) -> BTreeSet<String> {
    let no_comments: Vec<&str> = sql.lines().map(|l| l.split("--").next().unwrap()).collect();
    no_comments
        .join(" ")
        .split(';')
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Section 5 repeats the DDL of migrations v4 to v6; it is the same statements as `src/db/schema_v4.sql`, `schema_v5.sql` and `schema_v6.sql`.
#[test]
fn section_5_sql_is_migration_v4() {
    let s5 = section(SPEC, 5);
    let mut spec_sql = String::new();
    for block in s5.split("```sql").skip(1) {
        spec_sql.push_str(block.split("```").next().unwrap());
        spec_sql.push_str(";\n");
    }
    let spec_stmts = statements(&spec_sql);
    let file_stmts: BTreeSet<String> = statements(&format!(
        "{};{};{};{}",
        makiba::db::SCHEMA_V4,
        makiba::db::SCHEMA_V5,
        makiba::db::SCHEMA_V6,
        makiba::db::SCHEMA_V7
    ))
    .into_iter()
    .filter(|s| !(s == "BEGIN" || s == "COMMIT" || s.starts_with("INSERT INTO schema_migrations")))
    .collect();
    assert!(
        spec_stmts.len() >= 12,
        "read only {} statements from section 5",
        spec_stmts.len()
    );
    let only_spec: Vec<_> = spec_stmts.difference(&file_stmts).collect();
    let only_file: Vec<_> = file_stmts.difference(&spec_stmts).collect();
    assert!(
        only_spec.is_empty() && only_file.is_empty(),
        "section 5 and schema_v4.sql differ.\nonly in spec.md: {only_spec:#?}\nonly in schema_v4.sql and schema_v5.sql: {only_file:#?}"
    );
}
