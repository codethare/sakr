mod app;
mod bar;
mod config;
mod edge;
mod ipc;
mod notify;
mod preview;
mod render;
mod script;
mod wl;

use std::env;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        None => run_service(),
        Some("-h" | "--help") => {
            print!("{}", ipc::USAGE);
            ExitCode::SUCCESS
        }
        Some("--preview") => run_preview(),
        Some(_) => send_command(&args),
    }
}

fn run_service() -> ExitCode {
    let config = match config::load() {
        Ok((config, warnings)) => {
            for warning in warnings {
                eprintln!("quickbar: {warning}");
            }
            config
        }
        Err(error) => {
            eprintln!("quickbar: {error}");
            return ExitCode::FAILURE;
        }
    };

    match wl::run(config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("quickbar: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Send a command to a running service.
fn send_command(args: &[String]) -> ExitCode {
    match ipc::Command::parse(args) {
        Ok(command) => {
            eprintln!("quickbar: control channel is not implemented yet ({command:?})");
            ExitCode::FAILURE
        }
        Err(message) => {
            eprintln!("quickbar: {message}\n\n{}", ipc::USAGE);
            ExitCode::FAILURE
        }
    }
}

/// Render the edge samples to PNG files in the current directory.
fn run_preview() -> ExitCode {
    let config = match config::load() {
        Ok((config, warnings)) => {
            for warning in warnings {
                eprintln!("quickbar: {warning}");
            }
            config
        }
        Err(error) => {
            eprintln!("quickbar: {error}");
            return ExitCode::FAILURE;
        }
    };

    match preview::write_all(&config, Path::new(".")) {
        Ok(paths) => {
            for path in paths {
                println!("wrote {}", path.display());
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("quickbar: {error}");
            ExitCode::FAILURE
        }
    }
}
