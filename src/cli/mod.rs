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
mod image;
mod logs;
mod port;
mod ps;
mod scan;
mod sh;
mod up;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Debug, thiserror::Error)]
#[error("operation cancelled")]
pub struct UserCancelled;

pub(crate) fn prompt_error(error: inquire::error::InquireError) -> anyhow::Error {
    match error {
        inquire::error::InquireError::OperationCanceled
        | inquire::error::InquireError::OperationInterrupted => UserCancelled.into(),
        other => other.into(),
    }
}

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
    /// Search, list, and inspect OCI images
    Image(image::ImageArgs),
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
        Commands::Image(args) => image::run(args).await,
        Commands::Port(args) => port::run(args).await,
        Commands::Env(args) => env::run(args).await,
        Commands::Scan(args) => scan::run(args).await,
        Commands::Ignore(args) => ignore::run(args).await,
        Commands::Doctor(args) => doctor::run(args).await,
        Commands::Completions(args) => completions::run(args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_generic_image_runtime_controls() {
        let cli = Cli::try_parse_from([
            "asur",
            "up",
            "--image",
            "nginx:1.27-alpine",
            "--name",
            "web",
            "--publish",
            "127.0.0.1:8080:80/tcp",
            "--publish",
            "5353:53/udp",
            "--env",
            "MODE=dev",
            "--mount",
            "cache:/cache:ro",
            "--restart",
            "on-failure:3",
            "--command",
            "nginx",
            "--command",
            "-g",
            "--command",
            "daemon off;",
            "--pin",
        ])
        .unwrap();
        let Commands::Up(args) = cli.command else {
            panic!("expected up command");
        };
        assert_eq!(args.image.as_deref(), Some("nginx:1.27-alpine"));
        assert_eq!(args.publish.len(), 2);
        assert_eq!(args.publish[1].protocol, crate::docker::PortProtocol::Udp);
        assert_eq!(args.mount.len(), 1);
        assert_eq!(args.command, ["nginx", "-g", "daemon off;"]);
        assert!(args.pin);
    }

    #[test]
    fn rejects_invalid_port_mapping_before_runtime_work() {
        let result =
            Cli::try_parse_from(["asur", "up", "--image", "nginx:1.27", "--publish", "bad"]);
        let error = match result {
            Ok(_) => panic!("invalid port mapping should fail argument parsing"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("invalid port mapping"));
    }

    #[test]
    fn interrupted_prompts_become_clean_cancellation() {
        let error = prompt_error(inquire::error::InquireError::OperationInterrupted);
        assert!(error.downcast_ref::<UserCancelled>().is_some());
    }

    #[test]
    fn image_tag_singular_alias_is_accepted() {
        let cli = Cli::try_parse_from(["asur", "image", "tag", "typesense"]).unwrap();
        let Commands::Image(args) = cli.command else {
            panic!("expected image command");
        };
        assert!(matches!(
            args.command,
            image::ImageCommand::Tags {
                repository,
                limit: 10,
                all: false,
                json: false,
            } if repository == "typesense"
        ));
    }
}
