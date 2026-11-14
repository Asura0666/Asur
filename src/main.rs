mod catalog;
mod cli;
mod docker;
mod env;
mod scan;
mod ui;
mod util;

use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Parser;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    ui::configure(cli.plain, cli.no_color, cli.no_progress);
    let interactive = std::io::stdin().is_terminal();
    let result = tokio::select! {
        result = cli::run(cli) => result,
        signal = tokio::signal::ctrl_c() => match signal {
            Ok(()) => Err(cli::UserCancelled.into()),
            Err(error) => Err(error.into()),
        },
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) if err.downcast_ref::<cli::UserCancelled>().is_some() => {
            ui::info("Cancelled. Exiting without starting more work.");
            ExitCode::SUCCESS
        }
        Err(err) if interactive && err.downcast_ref::<docker::DockerUnavailable>().is_some() => {
            ui::warn(&format!("{err:#}"));
            ExitCode::SUCCESS
        }
        Err(err) => {
            ui::error(&format!("{err:#}"));
            ExitCode::FAILURE
        }
    }
}
