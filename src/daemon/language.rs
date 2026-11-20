//! The language settings (Issue 48, M6-7, carried from paddock's Q9; spec sections 12 and 13).
//!
//! A setting is the newest `config_set` event of its (project, key), `project` '' for the
//! organization. At a first launch one sentence of `roles/locale/` is appended to the task text:
//! the conversation language's to the secretary's, with the projects whose artifact language is not
//! English, and the project's artifact language (default `en`) to a worker's.

use rusqlite::Connection;
use std::collections::BTreeMap;

/// `(kind, code, sentence)`: the files of `roles/locale/<kind>/<code>.md`, carried in the binary.
const SENTENCES: [(&str, &str, &str); 4] = [
    (
        CONVERSATION,
        "en",
        include_str!("../../roles/locale/conversation/en.md"),
    ),
    (
        CONVERSATION,
        "ja",
        include_str!("../../roles/locale/conversation/ja.md"),
    ),
    (
        ARTIFACT,
        "en",
        include_str!("../../roles/locale/artifact/en.md"),
    ),
    (
        ARTIFACT,
        "ja",
        include_str!("../../roles/locale/artifact/ja.md"),
    ),
];

pub const CONVERSATION: &str = "conversation";
pub const ARTIFACT: &str = "artifact";

/// The keys built here, with the kind of their sentences and whether a project names them.
pub fn kind_of(key: &str) -> Option<(&'static str, bool)> {
    match key {
        "conversation_language" => Some((CONVERSATION, false)),
        "artifact_language" => Some((ARTIFACT, true)),
        _ => None,
    }
}

/// The sentence of `code` for `kind`, if `roles/locale/` has one.
pub fn sentence(kind: &str, code: &str) -> Option<&'static str> {
    SENTENCES
        .iter()
        .find(|(k, c, _)| *k == kind && *c == code)
        .map(|(_, _, s)| s.trim())
}

/// The codes that have a sentence of `kind`.
pub fn codes(kind: &str) -> Vec<&'static str> {
    SENTENCES
        .iter()
        .filter(|(k, _, _)| *k == kind)
        .map(|(_, c, _)| *c)
        .collect()
}

/// The stored value of `key` for `project` ('' for the organization): the newest `config_set`.
pub fn get(c: &Connection, project: &str, key: &str) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    c.query_row(
        "SELECT json_extract(payload_json, '$.value') FROM events
         WHERE kind = 'config_set' AND json_extract(payload_json, '$.project') = ?1
           AND json_extract(payload_json, '$.key') = ?2
         ORDER BY id DESC LIMIT 1",
        [project, key],
        |r| r.get(0),
    )
    .optional()
}

/// The language of `makibad`'s environment: the first non-empty of `LC_ALL`, `LC_MESSAGES`,
/// `LANG`, up to `_`, `.` or `@`; `C` and `POSIX` are English.
fn environment_language() -> Option<String> {
    let v = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|n| std::env::var(n).ok().filter(|v| !v.is_empty()))?;
    let code = v.split(['_', '.', '@']).next().unwrap_or("");
    Some(match code {
        "C" | "POSIX" => "en".into(),
        c => c.to_ascii_lowercase(),
    })
}

/// The projects whose artifact language is set to something other than `en`, with it.
fn non_english_projects(c: &Connection) -> rusqlite::Result<BTreeMap<String, String>> {
    let mut st = c.prepare(
        "SELECT json_extract(payload_json, '$.project'), json_extract(payload_json, '$.value')
         FROM events WHERE kind = 'config_set'
           AND json_extract(payload_json, '$.key') = 'artifact_language'
         ORDER BY id",
    )?;
    let mut all = BTreeMap::new();
    for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (p, v) = row?;
        all.insert(p, v);
    }
    all.retain(|_, v| v != "en");
    Ok(all)
}

/// The task text of a first launch of `role`: `brief`, then its language sentences.
pub fn task_text(
    c: &Connection,
    role: &str,
    project: Option<&str>,
    brief: &str,
) -> rusqlite::Result<String> {
    let mut parts = vec![brief.to_string()];
    match role {
        "secretary" => {
            let code = match get(c, "", "conversation_language")? {
                Some(v) => Some(v),
                None => environment_language(),
            };
            parts.extend(
                code.and_then(|v| sentence(CONVERSATION, &v))
                    .map(String::from),
            );
            let set = non_english_projects(c)?;
            if !set.is_empty() {
                let list: Vec<String> = set.iter().map(|(p, v)| format!("{p} ({v})")).collect();
                parts.push(format!(
                    "Projects whose artifact language is not English: {}.",
                    list.join(", ")
                ));
            }
        }
        "worker" => {
            let code = match project {
                Some(p) => get(c, p, "artifact_language")?,
                None => None,
            };
            parts.extend(sentence(ARTIFACT, code.as_deref().unwrap_or("en")).map(String::from));
        }
        _ => {}
    }
    Ok(parts.join("\n\n"))
}
