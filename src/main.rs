use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("daemon") => makiba::daemon::main(),
        Some("channel") => makiba::channel::main(&args[1..]),
        Some("status") => makiba::status_local::main(),
        _ => {
            print!("{}", makiba::USAGE);
            ExitCode::SUCCESS
        }
    }
}
