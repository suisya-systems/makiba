//! The test agent (M0-4): a program that behaves on command. Built as the binary
//! `makiba-fake-agent` (a `[[bin]]` entry in Cargo.toml); tests find it through
//! `CARGO_BIN_EXE_makiba-fake-agent`, and a release ships only `makiba`.
//!
//! usage: makiba-fake-agent --record FILE STEP...
//!
//! Steps, run in order:
//!   idle | working | done | error | none | blocked[:permission|question|auth[:MSG]]
//!                   report that state (`none`: no status) as an OSC 7501 record, with MSG as
//!                   its message, then print a visible line
//!   wait-line       read input until a line arrives
//!   wait-key        read input until a key arrives
//!   dialog          print a startup dialog with a cursor line; up and down move it, enter ends it
//!   menu:JSON       erase the screen (ESC [H ESC [2J) and draw one dialog frame from JSON
//!                   `{"above": [ROW...], "options": [TEXT...], "at": N, "indent": S,
//!                   "below": [ROW...]}`: the rows above, each option as INDENT + mark + space +
//!                   TEXT (the mark U+276F on the cursor's option, starting at N, a space on the
//!                   others), the rows below. Up and down move the cursor and draw the frame
//!                   again; enter erases the screen, records `chose N` (from 1) and prints
//!                   `[fake] dialog chose N`
//!   steps:FILE      run the steps of the JSON array of strings in FILE (relative to the working
//!                   folder, as the record file may be)
//!   prompt          report `blocked:permission`, print a permission prompt, wait for enter or esc
//!   sleep:MS        sleep
//!   exit[:N]        exit with code N (default 0)
//!   hang            never return
//!   env             record `env NAME=VALUE` for every variable of its environment and
//!                   `arg VALUE` for every argument, then `env-end`
//!   door:VERB:PATH  send VERB with no key to the door's Unix socket at PATH; record
//!                   `door VERB <answer line>`
//!   port:VERB:ADDR  call the tool VERB with no token on the door's MCP server at ADDR
//!                   (`POST /mcp`); record `port VERB <answer body>`
//!   detach:STEP     run STEP in a grandchild that has left this process: a hop process spawns it
//!                   in a new session (`setsid`) and exits at once. The grandchild waits until it
//!                   is re-parented, then records `detached ppid=P sid=S pid=Q agent=A hop=H
//!                   <STEP's record>`, where A is this process and H the hop. Neither the hop
//!                   nor the grandchild holds this process's input or output, and the
//!                   grandchild exits after 30 s at the latest
//!
//! `--mcp-config VALUE` among the steps is an argument the agent module adds (M3-4), not a step:
//! it is skipped, and `env` records it.
//!
//! Every line and key received is appended to FILE, one per row: `line TEXT` or `key NAME`
//! (`enter`, `up`, `down`, `left`, `right`, `esc`). The OSC 7501 record is
//! `ESC ] 7501 ; state=S:app=makiba-fake[:kind=K] BEL`.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::time::{Duration, Instant};

// libc symbols std already links; declared here so the test agent needs no crate of its own.
unsafe extern "C" {
    fn setsid() -> i32;
    fn getsid(pid: i32) -> i32;
}

/// Cursor home, then erase the display: what a program prints to draw a new frame.
const ERASE: &str = "\x1b[H\x1b[2J";

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

    /// `kind` is `KIND` or `KIND:MSG`; the message goes last, so it may hold a colon.
    fn report(&self, state: &str, kind: Option<&str>) {
        let kind = match kind.map(|k| k.split_once(':').unwrap_or((k, ""))) {
            Some((k, "")) => format!(":kind={k}"),
            Some((k, m)) => format!(":kind={k}:msg={m}"),
            None => String::new(),
        };
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

    /// The `menu` step.
    fn menu(&mut self, spec: &str) {
        let spec: serde_json::Value = serde_json::from_str(spec).expect("menu: JSON");
        let rows = |k: &str| -> Vec<String> {
            spec[k]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default()
        };
        let (above, options, below) = (rows("above"), rows("options"), rows("below"));
        let indent = spec["indent"].as_str().unwrap_or(" ").to_string();
        let last = options.len().saturating_sub(1);
        let mut at = (spec["at"].as_u64().unwrap_or(0) as usize).min(last);
        loop {
            print!("{ERASE}");
            for r in &above {
                self.say(r);
            }
            for (i, o) in options.iter().enumerate() {
                let mark = if i == at { "\u{276f}" } else { " " };
                self.say(&format!("{indent}{mark} {o}"));
            }
            for r in &below {
                self.say(r);
            }
            match self.next_input() {
                Some(Input::Key("down")) => at = (at + 1).min(last),
                Some(Input::Key("up")) => at = at.saturating_sub(1),
                Some(Input::Key("enter")) => break,
                Some(_) => {}
                None => std::process::exit(0),
            }
        }
        print!("{ERASE}");
        self.append(&format!("chose {}", at + 1));
        self.say(&format!("[fake] dialog chose {}", at + 1));
    }

    fn append(&self, row: &str) {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.record)
            .expect("open the record file");
        writeln!(f, "{row}").expect("write the record file");
    }

    fn step(&mut self, step: &str) {
        let (name, arg) = match step.split_once(':') {
            Some((n, a)) => (n, Some(a)),
            None => (step, None),
        };
        match name {
            "idle" | "working" | "done" | "error" | "none" => self.report(name, None),
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
            "menu" => self.menu(arg.unwrap_or("{}")),
            "steps" => {
                let path = arg.unwrap_or("");
                let text = std::fs::read_to_string(path).expect("read the steps file");
                let steps: Vec<String> = serde_json::from_str(&text).expect("a JSON array");
                for s in steps {
                    self.step(&s);
                }
            }
            "prompt" => {
                self.report("blocked", Some("permission"));
                self.choose("Do you want to proceed?", &["Yes", "No"]);
            }
            "sleep" => std::thread::sleep(std::time::Duration::from_millis(
                arg.and_then(|a| a.parse().ok()).unwrap_or(0),
            )),
            "exit" => std::process::exit(arg.and_then(|a| a.parse().ok()).unwrap_or(0)),
            "env" => {
                for (k, v) in std::env::vars_os() {
                    self.append(&format!(
                        "env {}={}",
                        k.to_string_lossy(),
                        v.to_string_lossy()
                    ));
                }
                for a in std::env::args() {
                    self.append(&format!("arg {a}"));
                }
                self.append("env-end");
            }
            "hang" => loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            },
            "door" | "port" => {
                let row = call(step);
                self.append(&row);
            }
            "detach" => {
                let me = std::env::current_exe().expect("own path");
                let hop = std::process::Command::new(me)
                    .args(["--record", &self.record, "--hop"])
                    .arg(std::process::id().to_string())
                    .arg(arg.unwrap_or(""))
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .expect("spawn the hop");
                assert!(hop.success(), "the hop failed");
            }
            _ => {
                eprintln!("makiba-fake-agent: unknown step {step:?}");
                std::process::exit(2);
            }
        }
    }
}

/// One call of a `door:` or `port:` step, with no key; returns the record row.
fn call(step: &str) -> String {
    let mut parts = step.splitn(3, ':');
    let (how, verb, at) = (
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
    );
    let answer = if how == "door" {
        door_call(verb, at)
    } else {
        port_call(verb, at)
    };
    let answer = answer.unwrap_or_else(|e| format!("error {e}"));
    format!("{how} {verb} {}", answer.replace(['\r', '\n'], " "))
}

fn door_call(verb: &str, path: &str) -> std::io::Result<String> {
    let mut s = std::os::unix::net::UnixStream::connect(path)?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    writeln!(s, "{{\"v\":1,\"verb\":\"{verb}\",\"args\":{{}}}}")?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line)?;
    Ok(line.trim_end().to_string())
}

fn port_call(verb: &str, addr: &str) -> std::io::Result<String> {
    let mut s = std::net::TcpStream::connect(addr)?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let body = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{{\"name\":\"{verb}\",\"arguments\":{{}}}}}}"
    );
    write!(
        s,
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    let mut all = String::new();
    s.read_to_string(&mut all)?;
    Ok(all
        .split_once("\r\n\r\n")
        .map_or(all.as_str(), |(_, b)| b)
        .to_string())
}

/// The hop of `detach`: start the grandchild in a new session and leave it at once.
fn hop(record: &str, agent: &str, step: &str) -> ! {
    let me = std::env::current_exe().expect("own path");
    let mut cmd = std::process::Command::new(me);
    cmd.args(["--record", record, "--orphan"])
        .arg(std::process::id().to_string())
        .args([agent, step])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: setsid is async-signal-safe and touches no memory of the parent.
    unsafe {
        cmd.pre_exec(|| {
            if setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().expect("spawn the grandchild");
    std::process::exit(0);
}

/// The grandchild of `detach`: wait to be re-parented away from the hop, then run the step.
fn orphan(record: &str, hop: &str, agent: &str, step: &str) {
    // Nobody waits for this process: it ends by itself whatever happens to the call.
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(30));
        std::process::exit(3);
    });
    let hop_pid: u32 = hop.parse().expect("hop pid");
    let until = Instant::now() + Duration::from_secs(10);
    while std::os::unix::process::parent_id() == hop_pid && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid = std::process::id();
    // SAFETY: getsid reads the session id of a process; no memory is shared.
    let sid = unsafe { getsid(0) };
    let ppid = std::os::unix::process::parent_id();
    let row = format!(
        "detached ppid={ppid} sid={sid} pid={pid} agent={agent} hop={hop} {}",
        call(step)
    );
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(record)
        .expect("open the record file");
    writeln!(f, "{row}").expect("write the record file");
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
    let rest: Vec<String> = args.collect();
    match rest.first().map(String::as_str) {
        Some("--hop") if rest.len() == 3 => hop(&record, &rest[1], &rest[2]),
        Some("--orphan") if rest.len() == 4 => {
            orphan(&record, &rest[1], &rest[2], &rest[3]);
            return;
        }
        _ => {}
    }
    // On a pty the line discipline holds a lone Esc until a newline; turn that off. On a pipe
    // (the stand-in) stty fails, which is fine.
    let _ = std::process::Command::new("stty")
        .args(["-icanon", "min", "1", "-echo"])
        .stderr(std::process::Stdio::null())
        .status();
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
    let mut steps = rest.into_iter();
    while let Some(step) = steps.next() {
        if step == "--mcp-config" {
            steps.next();
            continue;
        }
        agent.step(&step);
    }
}
