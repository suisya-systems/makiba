//! Issue 566: the nine words of the screen's drawing that had no key get one in
//! docs/design/screen.md section 2.4, in both wording files (roles/locale/screen/) and in the
//! drawing (docs/design/screen-draft.html). "stopped" is split: a failure nobody acts on is
//! "stuck"; a stop that asks the person keeps "Stopped to ask you". The texts are the ones the
//! owner approved (English, and the owner's Japanese).

use makiba::screen::Words;

const SCREEN_MD: &str = include_str!("../docs/design/screen.md");
const DRAWING: &str = include_str!("../docs/design/screen-draft.html");
const EN_TSV: &str = include_str!("../roles/locale/screen/en.tsv");

/// The keys of Issue 566 with their approved English and Japanese texts. The Japanese is written
/// as escapes: tools/check_language.py allows no Japanese under tests/ outside tests/fixtures/i18n/.
const WORDS: [(&str, &str, &str); 14] = [
    (
        "fail.fold_stuck",
        "{n} stuck ({x})",
        "\u{505c}\u{6b62}\u{4e2d} {n} \u{4ef6}\u{ff08}{x}\u{ff09}",
    ),
    (
        "fail.degree_stuck",
        "stuck, nobody on it",
        "\u{505c}\u{6b62}\u{4e2d}\u{3001}\u{8ab0}\u{3082}\u{5bfe}\u{5fdc}\u{3057}\u{3066}\u{3044}\u{306a}\u{3044}",
    ),
    (
        "range.fixes",
        "asks you after every {n} fixes of a task",
        "\u{4fee}\u{6b63} {n} \u{56de}\u{3054}\u{3068}\u{306b}\u{78ba}\u{8a8d}\u{3092}\u{6c42}\u{3081}\u{308b}",
    ),
    (
        "range.may_let_in",
        "It may let in",
        "\u{53d6}\u{308a}\u{8fbc}\u{3081}\u{308b}\u{7bc4}\u{56f2}",
    ),
    (
        "range.follow_ups",
        "New Issues found on the way",
        "\u{9014}\u{4e2d}\u{3067}\u{898b}\u{3064}\u{304b}\u{3063}\u{305f} Issue",
    ),
    (
        "range.follow_ups.inside",
        "let in without asking, from the same budget",
        "\u{78ba}\u{8a8d}\u{306a}\u{3057}\u{3067}\u{53d6}\u{308a}\u{8fbc}\u{3080}\u{ff08}\u{540c}\u{3058}\u{4e88}\u{7b97}\u{304b}\u{3089}\u{ff09}",
    ),
    (
        "range.follow_ups.ask",
        "asked of you first, as new approved work",
        "\u{53d6}\u{308a}\u{8fbc}\u{3080}\u{524d}\u{306b}\u{78ba}\u{8a8d}\u{3059}\u{308b}\u{ff08}\u{65b0}\u{3057}\u{3044}\u{627f}\u{8a8d}\u{3068}\u{3057}\u{3066}\u{ff09}",
    ),
    (
        "range.brief_head",
        "Every task starts from",
        "\u{5404}\u{30bf}\u{30b9}\u{30af}\u{306e}\u{6307}\u{793a}\u{306e}\u{5192}\u{982d}",
    ),
    ("ready.goal", "goal {x}", "\u{30b4}\u{30fc}\u{30eb} {x}"),
    (
        "fail.reason",
        "makiba's record: \u{201c}{x}\u{201d}",
        "makiba \u{306e}\u{8a18}\u{9332}\u{ff1a}\u{300c}{x}\u{300d}",
    ),
    (
        "issues.unchecked",
        "projects not checked: {n}",
        "\u{672a}\u{78ba}\u{8a8d}\u{306e}\u{30d7}\u{30ed}\u{30b8}\u{30a7}\u{30af}\u{30c8} {n} \u{4ef6}",
    ),
    (
        "task.kind.document",
        "document task",
        "\u{6587}\u{66f8}\u{30bf}\u{30b9}\u{30af}",
    ),
    (
        "task.kind.code",
        "code task",
        "\u{30b3}\u{30fc}\u{30c9}\u{30bf}\u{30b9}\u{30af}",
    ),
    (
        "fail.replaced",
        "runs a copy other than the one installed",
        "\u{30a4}\u{30f3}\u{30b9}\u{30c8}\u{30fc}\u{30eb}\u{6e08}\u{307f}\u{3068}\u{306f}\u{5225}\u{306e}\u{30d0}\u{30fc}\u{30b8}\u{30e7}\u{30f3}\u{3067}\u{52d5}\u{3044}\u{3066}\u{3044}\u{308b}",
    ),
];

#[test]
fn t1_each_key_has_its_approved_english_and_japanese() {
    let en = Words::new("en");
    let ja = Words::new("ja");
    for (key, want_en, want_ja) in WORDS {
        assert_eq!(en.get(key), want_en, "English of {key}");
        assert_eq!(ja.get(key), want_ja, "Japanese of {key}");
    }
}

#[test]
fn t2_only_a_stop_that_asks_you_or_a_prompt_says_stopped() {
    assert_eq!(Words::new("en").get("hist.asked"), "Stopped to ask you");
    let word = regex::Regex::new(r"(?i)\bstopped\b").unwrap();
    let mut found: Vec<&str> = EN_TSV
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('\t'))
        .filter(|(_, t)| word.is_match(t))
        .map(|(k, _)| k)
        .collect();
    found.sort_unstable();
    assert_eq!(
        found,
        [
            "fail.stuck_login",
            "fail.stuck_permission",
            "fail.stuck_question",
            "hist.asked",
            "term.blocked",
        ]
    );
}

#[test]
fn t3_screen_md_section_2_4_has_a_row_for_each_key() {
    let start = SCREEN_MD
        .find("### 2.4")
        .expect("screen.md has section 2.4");
    let end = start
        + SCREEN_MD[start..]
            .find("### 2.5")
            .expect("screen.md has section 2.5 after 2.4");
    let section = &SCREEN_MD[start..end];
    let keys = WORDS.iter().map(|(k, _, _)| *k).chain(["hist.asked"]);
    for key in keys {
        let row = format!("| `{key}` |");
        assert!(
            section.lines().any(|l| l.starts_with(&row)),
            "no row {row} in screen.md section 2.4"
        );
    }
}

#[test]
fn t4_the_drawing_uses_the_keys_words() {
    for gone in [
        "up to 2 fixes",
        " stopped (",
        "stopped, nobody on it",
        "Runs an older copy",
        "1 project unchecked",
    ] {
        assert!(!DRAWING.contains(gone), "the drawing still says {gone:?}");
    }
    for here in [
        "stuck, nobody on it",
        "goal G1",
        "document task",
        "projects not checked: 1",
        "New Issues found on the way",
    ] {
        assert!(DRAWING.contains(here), "the drawing does not say {here:?}");
    }
}
