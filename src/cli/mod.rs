// TODO: remove once every command reads all of its arguments.
#![allow(dead_code)]

//! Command-line interface: one module per command, wired together here.

mod clean;
mod completions;
mod db;
mod doctor;
mod down;
mod env;
mod ignore;
mod logs;
mod port;
mod ps;
mod scan;
mod sh;
mod up;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "asur",
    version,
    about = "One CLI for everyday local dev: containers, ports, .env files and leaked secrets.",
    propagate_version = true,
    arg_required_else_help = true
)]
pub struct Cli {
    /// Use simple line-oriented output without terminal decorations.
    #[arg(long, global = true)]
    pub plain: bool,
    /// Disable ANSI colors while preserving layout.
    #[arg(long, global = true)]
    pub no_color: bool,
    /// Disable animated spinners and progress bars.
    #[arg(long, global = true)]
    pub no_progress: bool,
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Spin up containers like postgres or redis with a guided wizard
    Up(up::UpArgs),
    /// Stop and remove containers
    Down(down::DownArgs),
    /// List containers in a readable table
    Ps(ps::PsArgs),
    /// Pick a container and view its logs
    Logs(logs::LogsArgs),
    /// Pick a container and open a shell inside it
    Sh(sh::ShArgs),
    /// Jump into psql, redis-cli, mysql or mongosh for a database container
    Db(db::DbArgs),
    /// Find and remove unused containers, images and volumes
    Clean(clean::CleanArgs),
    /// See who is using a port, or find the next free one
    Port(port::PortArgs),
    /// Work with .env files: generate .env.example, diff, validate
    Env(env::EnvArgs),
    /// Scan files and git history for leaked secrets
    Scan(scan::ScanArgs),
    /// Add .gitignore / .dockerignore entries based on the detected project type
    Ignore(ignore::IgnoreArgs),
    /// Diagnose common local-dev problems
    Doctor(doctor::DoctorArgs),
    /// Print shell completions (bash, zsh, fish, powershell, elvish)
    Completions(completions::CompletionsArgs),
}

pub async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Up(args) => up::run(args).await,
        Commands::Down(args) => down::run(args).await,
        Commands::Ps(args) => ps::run(args).await,
        Commands::Logs(args) => logs::run(args).await,
        Commands::Sh(args) => sh::run(args).await,
        Commands::Db(args) => db::run(args).await,
        Commands::Clean(args) => clean::run(args).await,
        Commands::Port(args) => port::run(args).await,
        Commands::Env(args) => env::run(args).await,
        Commands::Scan(args) => scan::run(args).await,
        Commands::Ignore(args) => ignore::run(args).await,
        Commands::Doctor(args) => doctor::run(args).await,
        Commands::Completions(args) => completions::run(args),
    }
}
