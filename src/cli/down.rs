//! Stop and remove containers (`asur down`).

use std::io::IsTerminal;

use anyhow::Result;
use clap::Args;
use owo_colors::{OwoColorize, Stream, Style};

use crate::docker::{ContainerInfo, ContainerRuntime, DockerRuntime};
use crate::ui;

#[derive(Args, Debug, Clone, Default)]
pub struct DownArgs {
    /// Containers to stop and remove (omit to select interactively)
    pub containers: Vec<String>,
    /// Only stop containers; do not remove them
    #[arg(long)]
    pub stop_only: bool,
    /// Also remove attached anonymous and named volumes
    #[arg(short = 'v', long)]
    pub volumes: bool,
    /// Skip the confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

pub async fn run(args: DownArgs) -> Result<()> {
    let runtime = DockerRuntime::connect()?;
    execute(args, &runtime).await
}

pub async fn execute<R: ContainerRuntime>(args: DownArgs, runtime: &R) -> Result<()> {
    let is_interactive = std::io::stdin().is_terminal();
    let all_containers = runtime.list_containers(true).await?;

    if all_containers.is_empty() {
        ui::info("No containers found.");
        return Ok(());
    }

    // Determine target container names
    let targets: Vec<String> = if !args.containers.is_empty() {
        args.containers.clone()
    } else if is_interactive {
        select_containers_interactive(&all_containers)?
    } else {
        ui::info(
            "No containers specified. Run `asur down <container>` or run interactively in a terminal.",
        );
        return Ok(());
    };

    if targets.is_empty() {
        ui::info("No containers selected.");
        return Ok(());
    }

    // Confirmation prompt (if not --yes and in interactive mode)
    if !args.yes && is_interactive {
        let action_desc = if args.stop_only {
            format!("Stop {} container(s)", targets.len())
        } else if args.volumes {
            format!(
                "Stop and REMOVE {} container(s) and their VOLUMES",
                targets.len()
            )
        } else {
            format!("Stop and remove {} container(s)", targets.len())
        };

        let names_list = targets.join(", ");
        let prompt_msg = format!("{action_desc} ({names_list})?");

        let confirmed = inquire::Confirm::new(&prompt_msg)
            .with_default(false)
            .prompt()
            .unwrap_or(false);

        if !confirmed {
            ui::info("Cancelled.");
            return Ok(());
        }
    }

    let green_bold = Style::new().green().bold();

    for name in &targets {
        // Check if container is running
        let is_running = all_containers
            .iter()
            .find(|c| c.name == *name)
            .map(|c| c.state == "running")
            .unwrap_or(true);

        if is_running {
            ui::info(&format!("Stopping `{name}`..."));
            if let Err(e) = runtime.stop_container(name).await {
                ui::warn(&format!("Failed to stop `{name}`: {e}"));
            } else {
                let check = "✔".if_supports_color(Stream::Stdout, |t| t.style(green_bold));
                println!("  {check} Stopped `{name}`");
            }
        }

        if !args.stop_only {
            ui::info(&format!("Removing `{name}`..."));
            if let Err(e) = runtime.remove_container(name, args.volumes).await {
                ui::error(&format!("Failed to remove `{name}`: {e}"));
            } else {
                let check = "✔".if_supports_color(Stream::Stdout, |t| t.style(green_bold));
                let vol_msg = if args.volumes {
                    " (including volumes)"
                } else {
                    ""
                };
                println!("  {check} Removed `{name}`{vol_msg}");
            }
        }
    }

    println!();
    ui::info(&format!("Done. {} container(s) processed.", targets.len()));

    Ok(())
}

/// Interactive multi-selection prompt showing container name, image, and status.
fn select_containers_interactive(containers: &[ContainerInfo]) -> Result<Vec<String>> {
    let options: Vec<String> = containers
        .iter()
        .map(|c| format!("{:<20} {:<24} ({})", c.name, c.image, c.status))
        .collect();

    let selected = inquire::MultiSelect::new(
        "Select container(s) to take down (Space to select, Enter to confirm):",
        options,
    )
    .prompt()
    .map_err(|e| anyhow::anyhow!("selection cancelled: {e}"))?;

    let names = selected
        .into_iter()
        .filter_map(|s| s.split_whitespace().next().map(str::to_string))
        .collect();

    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeDownRuntime {
        containers: Vec<ContainerInfo>,
        stopped: Mutex<Vec<String>>,
        removed: Mutex<Vec<(String, bool)>>,
    }

    impl FakeDownRuntime {
        fn new(containers: Vec<ContainerInfo>) -> Self {
            Self {
                containers,
                stopped: Mutex::new(Vec::new()),
                removed: Mutex::new(Vec::new()),
            }
        }
    }

    impl ContainerRuntime for FakeDownRuntime {
        async fn list_containers(&self, _all: bool) -> Result<Vec<ContainerInfo>> {
            Ok(self.containers.clone())
        }

        async fn run_container(
            &self,
            _config: crate::docker::CreateContainerConfig,
        ) -> Result<ContainerInfo> {
            unimplemented!()
        }

        async fn stop_container(&self, id_or_name: &str) -> Result<()> {
            self.stopped.lock().unwrap().push(id_or_name.to_string());
            Ok(())
        }

        async fn remove_container(&self, id_or_name: &str, remove_volumes: bool) -> Result<()> {
            self.removed
                .lock()
                .unwrap()
                .push((id_or_name.to_string(), remove_volumes));
            Ok(())
        }

        async fn stream_logs(
            &self,
            _id_or_name: &str,
            _tail: usize,
            _follow: bool,
            _tx: tokio::sync::mpsc::UnboundedSender<String>,
        ) -> Result<()> {
            Ok(())
        }

        async fn get_clean_report(&self) -> Result<crate::docker::CleanReport> {
            Ok(crate::docker::CleanReport::default())
        }

        async fn clean_resources(
            &self,
            _container_ids: &[String],
            _image_ids: &[String],
            _volume_names: &[String],
        ) -> Result<crate::docker::CleanResults> {
            Ok(crate::docker::CleanResults::default())
        }

        async fn get_container_details(
            &self,
            id_or_name: &str,
        ) -> Result<crate::docker::ContainerDetails> {
            Ok(crate::docker::ContainerDetails {
                name: id_or_name.to_string(),
                image: "redis:7".to_string(),
                env: vec![],
            })
        }
    }

    fn sample_running_containers() -> Vec<ContainerInfo> {
        vec![
            ContainerInfo {
                name: "redis-1".into(),
                image: "redis:7-alpine".into(),
                status: "Up 2 minutes".into(),
                state: "running".into(),
                ports: vec!["0.0.0.0:6380->6379/tcp".into()],
                created: 100,
            },
            ContainerInfo {
                name: "postgres-1".into(),
                image: "postgres:16-alpine".into(),
                status: "Up 1 minute".into(),
                state: "running".into(),
                ports: vec!["0.0.0.0:5433->5432/tcp".into()],
                created: 150,
            },
        ]
    }

    #[tokio::test]
    async fn test_down_specific_container_stops_and_removes() {
        let fake = FakeDownRuntime::new(sample_running_containers());
        let args = DownArgs {
            containers: vec!["redis-1".into()],
            stop_only: false,
            volumes: false,
            yes: true,
        };

        execute(args, &fake).await.unwrap();

        assert_eq!(*fake.stopped.lock().unwrap(), vec!["redis-1"]);
        assert_eq!(
            *fake.removed.lock().unwrap(),
            vec![("redis-1".to_string(), false)]
        );
    }

    #[tokio::test]
    async fn test_down_stop_only_does_not_remove() {
        let fake = FakeDownRuntime::new(sample_running_containers());
        let args = DownArgs {
            containers: vec!["postgres-1".into()],
            stop_only: true,
            volumes: false,
            yes: true,
        };

        execute(args, &fake).await.unwrap();

        assert_eq!(*fake.stopped.lock().unwrap(), vec!["postgres-1"]);
        assert!(fake.removed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_down_with_volumes() {
        let fake = FakeDownRuntime::new(sample_running_containers());
        let args = DownArgs {
            containers: vec!["redis-1".into()],
            stop_only: false,
            volumes: true,
            yes: true,
        };

        execute(args, &fake).await.unwrap();

        assert_eq!(
            *fake.removed.lock().unwrap(),
            vec![("redis-1".to_string(), true)]
        );
    }

    #[tokio::test]
    async fn test_down_empty_list_returns_cleanly() {
        let fake = FakeDownRuntime::new(vec![]);
        let args = DownArgs {
            containers: vec![],
            stop_only: false,
            volumes: false,
            yes: true,
        };

        let result = execute(args, &fake).await;
        assert!(result.is_ok());
    }
}
