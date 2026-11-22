//! Issue 269: the premise-check hook also reads the Japanese the person may write. The messages
//! are written as `\u{..}` escapes so that this file stays ASCII.

use std::io::Write;
use std::process::{Command, Stdio};

fn hook(text: &str) -> (i32, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_makiba"))
        .args(["hook", "premise-check"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let payload = serde_json::json!({"prompt": text}).to_string();
    c.stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = c.wait_with_output().unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn a_japanese_message_with_a_claim_gets_the_line() {
    for text in [
        // "the timeout is 9 minutes" with the number glued to Japanese text
        "\u{30BF}\u{30A4}\u{30E0}\u{30A2}\u{30A6}\u{30C8}\u{306F}9\u{5206}\u{3067}\u{3059}\u{3088}\u{306D}",
        // "the threshold should assume a 256K window"
        "\u{95BE}\u{5024}\u{306F}256K\u{306E}\u{7A93}\u{3092}\u{524D}\u{63D0}\u{306B}",
        // a setting glued to Japanese text: "set TIMEOUT=900"
        "\u{8A2D}\u{5B9A}TIMEOUT=900\u{306B}\u{3057}\u{3066}",
        // a claim word only: "the default is ... isn't it"
        "\u{30C7}\u{30D5}\u{30A9}\u{30EB}\u{30C8}\u{306F}\u{3053}\u{3046}\u{306E}\u{306F}\u{305A}",
    ] {
        let (code, out) = hook(text);
        assert_eq!(code, 0, "{text}");
        assert!(out.starts_with("VERIFY_BEFORE_AGREE"), "{text}: {out:?}");
    }
}

#[test]
fn a_japanese_message_without_a_claim_gets_nothing() {
    // "please look at the open pull requests", "thanks"
    for text in [
        "\u{30AA}\u{30FC}\u{30D7}\u{30F3}\u{306A}PR\u{3092}\u{898B}\u{3066}\u{304F}\u{3060}\u{3055}\u{3044}",
        "\u{3042}\u{308A}\u{304C}\u{3068}\u{3046}",
    ] {
        assert_eq!(hook(text), (0, String::new()), "{text}");
    }
}

#[test]
fn a_tag_question_alone_gets_the_line() {
    for text in [
        "Retries are disabled, right?",
        "Retries are disabled, right? ",
    ] {
        let (code, out) = hook(text);
        assert_eq!(code, 0, "{text}");
        assert!(out.starts_with("VERIFY_BEFORE_AGREE"), "{text}: {out:?}");
    }
}
