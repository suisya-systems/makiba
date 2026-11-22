//! `makiba hook premise-check` (Issue 269, incident I-11): a `UserPromptSubmit` hook on the
//! secretary. When the person's message carries a claim (a number with a unit, a `key=value`
//! setting, or a word that states how something is or should be), it prints one reminder line,
//! which Claude Code adds to the secretary's context. Ported from claude-org-ja's
//! `tools/premise_check.py` (Issue #1192): its Japanese units and claim words are kept, written as
//! `\x{..}` escapes so that this file stays ASCII, and English ones are added.
//!
//! It reminds; it does not verify. It cannot see whether a record was opened, and it never
//! refuses: it exits 0 on every input, malformed ones too, because a hook that blocked the
//! person's own message would be worse than the lapse it guards against.
//!
//! Known false positives, each costing one extra line: a port such as `localhost:8080`, a
//! sentence with "default" or "limit" that states no claim, a count in a request ("add 3 tests").
//! Task ids (`makiba-269-x`), pull request numbers (`#269`), dates (`2026-11-22`) and commit
//! hashes carry no unit or `=` and do not match. Messages wrapped for the secretary
//! (`<makiba `, `<channel source=`, `<task-notification>`) are not the person's words and are
//! skipped.

use regex::Regex;
use serde_json::Value;
use std::io::Read;
use std::process::ExitCode;
use std::sync::OnceLock;

pub const REMINDER: &str = "VERIFY_BEFORE_AGREE: the person's message states a number, a setting or a behaviour. Before you agree or disagree, open the record or the code once and answer with its source.";

fn patterns() -> &'static [Regex; 3] {
    static P: OnceLock<[Regex; 3]> = OnceLock::new();
    // `(?-u:\b)` is an ASCII word boundary: a Japanese character next to a number or a word
    // (`9\u5206`, `\u8A2D\u5B9ATIMEOUT=9`) does not hide it. The Japanese words are written as
    // `\x{....}` so that the file stays ASCII: \u79D2 sec, \u5206 min, \u6642\u9593 hour,
    // \u65E5 day, \u9031 week, \u30C8\u30FC\u30AF\u30F3 token, \u500D times, \u884C line.
    const B: &str = r"(?-u:\b)";
    P.get_or_init(|| {
        [
            // A number with a unit: "256K", "80%", "9 minutes", "9\u5206", "746,933 tokens".
            Regex::new(&format!(
                r"(?i)\d[\d,.]*\s*(?:[kmg]{B}|[kmg]b{B}|%|\x{{FF05}}|ms{B}|secs?{B}|seconds?{B}|mins?{B}|minutes?{B}|hours?{B}|days?{B}|weeks?{B}|tokens?{B}|x{B}|times{B}|lines?{B}|\x{{79D2}}|\x{{5206}}|\x{{6642}}\x{{9593}}|\x{{65E5}}|\x{{9031}}|\x{{30C8}}\x{{30FC}}\x{{30AF}}\x{{30F3}}|\x{{500D}}|\x{{884C}})"
            ))
            .unwrap(),
            // A setting: "TRANSPORT=broker", "timeout: 900".
            Regex::new(r#"[A-Za-z_][\w.-]*\s*=\s*[\w"'./-]|[A-Za-z_][\w.-]*: *\d"#).unwrap(),
            // A word that states how something is or should be.
            Regex::new(&format!(
                r"(?i){B}(?:should be|should assume|is set to|defaults? to|the default|threshold|upper limit|lower limit|premise|assum(?:e|ing|ption)|supposed to|isn't it|right\?|specification){B}|\x{{306F}}\x{{305A}}|\x{{306E}}\x{{3067}}\x{{306F}}|\x{{3058}}\x{{3083}}\x{{306A}}\x{{3044}}[\x{{FF1F}}?]|\x{{3067}}\x{{3059}}\x{{3088}}\x{{306D}}|\x{{3060}}\x{{3088}}\x{{306D}}|\x{{3067}}\x{{3057}}\x{{3087}}|\x{{524D}}\x{{63D0}}|\x{{4ED5}}\x{{69D8}}|\x{{65E2}}\x{{5B9A}}|\x{{30C7}}\x{{30D5}}\x{{30A9}}\x{{30EB}}\x{{30C8}}|\x{{95BE}}\x{{5024}}|\x{{4E0A}}\x{{9650}}|\x{{4E0B}}\x{{9650}}|\x{{8A2D}}\x{{5B9A}}\x{{5024}}"
            ))
            .unwrap(),
        ]
    })
}

pub fn has_premise(prompt: &str) -> bool {
    if ["<makiba ", "<channel source=", "<task-notification>"]
        .iter()
        .any(|w| prompt.contains(w))
    {
        return false;
    }
    patterns().iter().any(|p| p.is_match(prompt))
}

/// Read the payload from standard input; print the reminder when `prompt` carries a claim.
/// Always exit 0.
pub fn main() -> ExitCode {
    let mut raw = Vec::new();
    if std::io::stdin().read_to_end(&mut raw).is_ok()
        && let Ok(Value::Object(p)) = serde_json::from_slice::<Value>(&raw)
        && let Some(Value::String(prompt)) = p.get("prompt")
        && has_premise(prompt)
    {
        println!("{REMINDER}");
    }
    ExitCode::SUCCESS
}
