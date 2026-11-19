use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("daemon") => makiba::daemon::main(),
        Some("channel") => makiba::channel::main(&args[1..]),
        Some("hook") => makiba::hooks::main(&args[1..]),
        Some("status") => makiba::status_local::main(),
        Some("send" | "report" | "show") => makiba::cli::main(&args),
        Some("up") => makiba::up::main(&args[1..]),
        _ => {
            print!("{}", makiba::USAGE);
            ExitCode::SUCCESS
        }
    }
}
