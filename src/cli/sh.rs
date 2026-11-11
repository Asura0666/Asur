//! Open an interactive shell inside a container (`asur sh`).

use std::io::IsTerminal;
use std::process::Command;

use anyhow::{Context, Result};
use clap::Args;

use crate::docker::{ContainerInfo, ContainerRuntime, DockerRuntime};
use crate::ui;

#[derive(Args, Debug, Clone, Default)]
pub struct ShArgs {
    /// Container name or id (omit to pick from a list)
    pub container: Option<String>,
}

pub async fn run(args: ShArgs) -> Result<()> {
    let runtime = DockerRuntime::connect()?;
    execute(args, &runtime).await
}

pub async fn execute<R: ContainerRuntime>(args: ShArgs, runtime: &R) -> Result<()> {
    let is_interactive = std::io::stdin().is_terminal();
    let all = runtime.list_containers(false).await?; // only running containers

    if all.is_empty() {
        ui::info("No running containers found. Run `asur ps` to check container status.");
        return Ok(());
    }

    let target = if let Some(c) = args.container {
        c
    } else if is_interactive {
        select_running_container_interactive(&all)?
    } else {
        ui::info(
            "No container specified. Run `asur sh <container>` or run in an interactive terminal.",
        );
        return Ok(());
    };

    ui::info(&format!("Connecting to shell in `{target}`..."));

    // Try bash first, then fallback to sh
    let status = Command::new("docker")
        .args(["exec", "-it", &target, "bash"])
        .status();

    match status {
        Ok(s) if s.code() == Some(126) || s.code() == Some(127) => {
            // bash not found in container, fall back to sh
            let sh_status = Command::new("docker")
                .args(["exec", "-it", &target, "sh"])
                .status()
                .context("failed to start shell inside container")?;

            if !sh_status.success() {
                ui::warn(&format!("Shell exited with code {:?}", sh_status.code()));
            }
        }
        Ok(s) => {
            if !s.success() && s.code() != Some(130) {
                // 130 is normal exit via SIGINT (Ctrl+C)
                ui::warn(&format!("Shell exited with code {:?}", s.code()));
            }
        }
        Err(e) => {
            return Err(anyhow::anyhow!(
                "could not launch interactive terminal: {e}.\n\
                 Make sure `docker` CLI is available in your PATH for interactive terminal sessions."
            ));
        }
    }

    Ok(())
}

fn select_running_container_interactive(containers: &[ContainerInfo]) -> Result<String> {
    let options: Vec<String> = containers
        .iter()
        .map(|c| format!("{:<20} {:<24} ({})", c.name, c.image, c.status))
        .collect();

    let choice = inquire::Select::new("Select a running container to open a shell in:", options)
        .prompt()
        .map_err(|e| anyhow::anyhow!("selection cancelled: {e}"))?;

    let name = choice
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("invalid selection"))?;

    Ok(name.to_string())
}
