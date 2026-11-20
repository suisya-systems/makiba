use clap::Parser;
use makiba::cli::{Command, Makiba};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Not the person's verbs: started by makiba itself and by Claude Code (spec section 14).
    match args.first().map(String::as_str) {
        Some("daemon") => return makiba::daemon::main(),
        Some("channel") => return makiba::channel::main(&args[1..]),
        Some("hook") => return makiba::hooks::main(&args[1..]),
        _ => {}
    }
    let parsed = match Makiba::try_parse_from(std::env::args()) {
        Ok(m) => m,
        Err(e) => {
            let _ = e.print();
            return makiba::cli::exit(e.exit_code());
        }
    };
    match parsed.verb {
        Command::Up { .. } => makiba::up::main(&args[1..]),
        Command::Down { .. } => makiba::down::main(&args[1..]),
        Command::Status { json } => makiba::status_local::main(json),
        other => makiba::cli::main(other),
    }
}
