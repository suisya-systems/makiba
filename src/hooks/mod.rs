//! `makiba hook <name>`: the Claude Code hooks a role's settings name (spec section 14). Each reads
//! the hook payload on standard input; exit 0 lets the tool call run, exit 2 refuses it with the
//! reason on standard error.

use std::process::ExitCode;

pub mod deny_base;

pub fn main(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("deny-base") => deny_base::main(),
        _ => {
            eprintln!("usage: makiba hook deny-base");
            ExitCode::from(2)
        }
    }
}
