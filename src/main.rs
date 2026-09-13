mod app;
mod bar;
mod config;
mod edge;
mod ipc;
mod notify;
mod render;
mod script;
mod wl;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        None => {
            eprintln!("quickbar: service mode is not implemented yet");
            ExitCode::FAILURE
        }
        Some("-h" | "--help") => {
            print!("{}", ipc::USAGE);
            ExitCode::SUCCESS
        }
        Some(_) => match ipc::Command::parse(&args) {
            Ok(command) => {
                eprintln!("quickbar: control channel is not implemented yet ({command:?})");
                ExitCode::FAILURE
            }
            Err(message) => {
                eprintln!("quickbar: {message}\n\n{}", ipc::USAGE);
                ExitCode::FAILURE
            }
        },
    }
}
