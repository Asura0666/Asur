mod catalog;
mod cli;
mod docker;
mod env;
mod scan;
mod ui;
mod util;

use std::process::ExitCode;

use clap::Parser;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    ui::configure(cli.plain, cli.no_color, cli.no_progress);
    match cli::run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            ui::error(&format!("{err:#}"));
            ExitCode::FAILURE
        }
    }
}
