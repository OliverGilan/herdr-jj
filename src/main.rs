mod config;
mod herdr;
mod jj;
mod process;
mod ui;
mod workflows;

use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let interactive = args.first().is_some_and(|value| value == "pane");
    let result: anyhow::Result<()> = match args.as_slice() {
        [command] if command == "refresh" => workflows::refresh_status(),
        [command, action] if command == "action" => workflows::open_action(action),
        [command, pane] if command == "pane" => workflows::run_pane(pane),
        _ => Err(anyhow::anyhow!(
            "usage: herdr-jj <refresh | action <create|open|remove> | pane <create|open|remove>>"
        )),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            if interactive {
                eprint!("\nPress Enter to close...");
                let _ = io::stderr().flush();
                let mut line = String::new();
                let _ = io::stdin().read_line(&mut line);
            }
            ExitCode::FAILURE
        }
    }
}
