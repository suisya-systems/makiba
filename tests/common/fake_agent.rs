//! The test agent (M0-4): a program that behaves on command. Built as the binary
//! `makiba-fake-agent` (a `[[bin]]` entry in Cargo.toml); tests find it through
//! `CARGO_BIN_EXE_makiba-fake-agent`, and a release ships only `makiba`.
//!
//! usage: makiba-fake-agent --record FILE STEP...
//!
//! Steps, run in order:
//!   idle | working | done | error | blocked[:permission|question|auth]
//!                   report that state as an OSC 7501 record, then print a visible line
//!   wait-line       read input until a line arrives
//!   wait-key        read input until a key arrives
//!   dialog          print a startup dialog with a cursor line; up and down move it, enter ends it
//!   prompt          report `blocked:permission`, print a permission prompt, wait for enter or esc
//!   sleep:MS        sleep
//!   exit[:N]        exit with code N (default 0)
//!   hang            never return
//!
//! Every line and key received is appended to FILE, one per row: `line TEXT` or `key NAME`
//! (`enter`, `up`, `down`, `left`, `right`, `esc`). The OSC 7501 record is
//! `ESC ] 7501 ; state=S:app=makiba-fake[:kind=K] BEL`.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::time::Duration;

enum Input {
    Line(String),
    Key(&'static str),
}

struct Agent {
    record: String,
    bytes: std::sync::mpsc::Receiver<u8>,
    pushed_back: Option<u8>,
}

impl Agent {
    fn say(&self, text: &str) {
        println!("{text}");
        let _ = std::io::stdout().flush();
    }

    fn report(&self, state: &str, kind: Option<&str>) {
        let kind = kind.map(|k| format!(":kind={k}")).unwrap_or_default();
        print!("\x1b]7501;state={state}:app=makiba-fake{kind}\x07");
        self.say(&format!("[fake] {state}"));
    }

    fn byte(&mut self) -> Option<u8> {
        if let Some(b) = self.pushed_back.take() {
            return Some(b);
        }
        self.bytes.recv().ok()
    }

    /// The next byte, or `None` if none comes within a short time (or at the end of input).
    fn byte_soon(&mut self) -> Option<u8> {
        self.bytes.recv_timeout(Duration::from_millis(50)).ok()
    }

    /// The next line or key. `None` at the end of input.
    fn next_input(&mut self) -> Option<Input> {
        let mut text: Vec<u8> = Vec::new();
        loop {
            let b = self.byte()?;
            let input = match b {
                b'\r' | b'\n' if text.is_empty() => Input::Key("enter"),
                b'\r' | b'\n' => {
                    Input::Line(String::from_utf8_lossy(&std::mem::take(&mut text)).into_owned())
                }
                0x1b => match self.byte_soon() {
                    // ponytail: a lone Esc is told by 50 ms of quiet; a real key sequence
                    // arrives in one write, so the gap never shows inside one.
                    Some(b'[') => match self.byte_soon() {
                        Some(b'A') => Input::Key("up"),
                        Some(b'B') => Input::Key("down"),
                        Some(b'C') => Input::Key("right"),
                        Some(b'D') => Input::Key("left"),
                        _ => continue,
                    },
                    other => {
                        self.pushed_back = other;
                        Input::Key("esc")
                    }
                },
                _ => {
                    text.push(b);
                    continue;
                }
            };
            let row = match &input {
                Input::Line(l) => format!("line {l}"),
                Input::Key(k) => format!("key {k}"),
            };
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.record)
                .expect("open the record file");
            writeln!(f, "{row}").expect("write the record file");
            return Some(input);
        }
    }

    fn wait_line(&mut self) {
        loop {
            match self.next_input() {
                Some(Input::Line(_)) => return,
                Some(Input::Key(_)) => {}
                None => std::process::exit(0),
            }
        }
    }

    fn wait_key(&mut self) {
        loop {
            match self.next_input() {
                Some(Input::Key(_)) => return,
                Some(Input::Line(_)) => {}
                None => std::process::exit(0),
            }
        }
    }

    fn show_options(&self, options: &[&str], at: usize) {
        for (i, o) in options.iter().enumerate() {
            let mark = if i == at { "❯" } else { " " };
            self.say(&format!("{mark} {}. {o}", i + 1));
        }
    }

    /// Print `question` and `options`, move the cursor with up and down (printing the options
    /// again each time), and end on enter. Returns the chosen index.
    fn choose(&mut self, question: &str, options: &[&str]) -> usize {
        let mut at = 0;
        self.say(question);
        self.show_options(options, at);
        loop {
            match self.next_input() {
                Some(Input::Key("down")) => at = (at + 1).min(options.len() - 1),
                Some(Input::Key("up")) => at = at.saturating_sub(1),
                Some(Input::Key("enter")) => return at,
                Some(Input::Key("esc")) => return options.len() - 1,
                Some(_) => continue,
                None => std::process::exit(0),
            }
            self.show_options(options, at);
        }
    }

    fn step(&mut self, step: &str) {
        let (name, arg) = match step.split_once(':') {
            Some((n, a)) => (n, Some(a)),
            None => (step, None),
        };
        match name {
            "idle" | "working" | "done" | "error" => self.report(name, None),
            "blocked" => self.report("blocked", arg),
            "wait-line" => self.wait_line(),
            "wait-key" => self.wait_key(),
            "dialog" => {
                let at = self.choose(
                    "Do you trust the files in this folder?",
                    &["Yes, I trust this folder", "No, exit"],
                );
                self.say(&format!("[fake] dialog chose {}", at + 1));
            }
            "prompt" => {
                self.report("blocked", Some("permission"));
                self.choose("Do you want to proceed?", &["Yes", "No"]);
            }
            "sleep" => std::thread::sleep(std::time::Duration::from_millis(
                arg.and_then(|a| a.parse().ok()).unwrap_or(0),
            )),
            "exit" => std::process::exit(arg.and_then(|a| a.parse().ok()).unwrap_or(0)),
            "hang" => loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            },
            _ => {
                eprintln!("makiba-fake-agent: unknown step {step:?}");
                std::process::exit(2);
            }
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("--record") {
        eprintln!("usage: makiba-fake-agent --record FILE STEP...");
        std::process::exit(2);
    }
    let Some(record) = args.next() else {
        eprintln!("usage: makiba-fake-agent --record FILE STEP...");
        std::process::exit(2);
    };
    // One thread owns stdin so a read can be waited on with a timeout.
    let (tx, bytes) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for b in std::io::stdin().lock().bytes().map_while(Result::ok) {
            if tx.send(b).is_err() {
                break;
            }
        }
    });
    let mut agent = Agent {
        record,
        bytes,
        pushed_back: None,
    };
    for step in args {
        agent.step(&step);
    }
}
