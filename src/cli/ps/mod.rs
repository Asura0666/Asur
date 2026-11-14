mod format;

use anyhow::Result;
use clap::Args;

use crate::docker::{ContainerRuntime, DockerRuntime};

#[derive(Args, Debug, Clone, Default)]
pub struct PsArgs {
    /// Include stopped containers (default: true)
    #[arg(short, long)]
    pub all: bool,
    /// Only show running containers
    #[arg(short, long)]
    pub running: bool,
    /// Machine-readable JSON output (no colours, no extra text)
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: PsArgs) -> Result<()> {
    let runtime = DockerRuntime::connect()?;
    let output = execute(args, &runtime).await?;
    println!("{output}");
    Ok(())
}

/// Execute `ps` against any [`ContainerRuntime`] and format the output.
pub async fn execute<R: ContainerRuntime>(args: PsArgs, runtime: &R) -> Result<String> {
    // Show all containers by default (including stopped/exited), unless `--running` is specified
    let show_all = !args.running;
    let containers = runtime.list_containers(show_all).await?;

    if args.json {
        format::render_json(&containers)
    } else {
        Ok(format::render_table(&containers))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::ContainerInfo;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    struct FakeRuntime {
        containers: Vec<ContainerInfo>,
        all_passed: AtomicBool,
    }

    impl FakeRuntime {
        fn new(containers: Vec<ContainerInfo>) -> Self {
            Self {
                containers,
                all_passed: AtomicBool::new(false),
            }
        }
    }

    impl ContainerRuntime for FakeRuntime {
        async fn list_containers(&self, all: bool) -> Result<Vec<ContainerInfo>> {
            self.all_passed.store(all, Ordering::SeqCst);
            if all {
                Ok(self.containers.clone())
            } else {
                Ok(self
                    .containers
                    .iter()
                    .filter(|c| c.state == "running")
                    .cloned()
                    .collect())
            }
        }

        async fn run_container(
            &self,
            config: crate::docker::CreateContainerConfig,
        ) -> Result<ContainerInfo> {
            Ok(ContainerInfo {
                name: config.name,
                image: config.image,
                status: "Up just now".into(),
                state: "running".into(),
                ports: config
                    .ports
                    .iter()
                    .map(|binding| {
                        format!(
                            "0.0.0.0:{}->{}/{}",
                            binding.host_port, binding.container_port, binding.protocol
                        )
                    })
                    .collect(),
                created: 1000,
            })
        }

        async fn stop_container(&self, _id_or_name: &str) -> Result<()> {
            Ok(())
        }

        async fn remove_container(&self, _id_or_name: &str, _remove_volumes: bool) -> Result<()> {
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
                image: "postgres:16".to_string(),
                env: vec![],
            })
        }
    }

    fn sample_containers() -> Vec<ContainerInfo> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs() as i64;

        vec![
            ContainerInfo {
                name: "dev-postgres".into(),
                image: "postgres:16".into(),
                status: "Up 2 hours".into(),
                state: "running".into(),
                ports: vec!["0.0.0.0:5432->5432/tcp".into()],
                created: now - 7200,
            },
            ContainerInfo {
                name: "dev-redis".into(),
                image: "redis:7".into(),
                status: "Exited (0) 10 minutes ago".into(),
                state: "exited".into(),
                ports: vec!["6379/tcp".into()],
                created: now - 3600,
            },
        ]
    }

    #[tokio::test]
    async fn test_ps_table_shows_all_by_default() {
        let fake = FakeRuntime::new(sample_containers());
        let args = PsArgs::default();

        let output = execute(args, &fake).await.unwrap();
        assert!(fake.all_passed.load(Ordering::SeqCst));
        assert!(output.contains("dev-postgres"));
        assert!(output.contains("dev-redis"));
        assert!(output.contains("Exited (0) 10 minutes ago"));
    }

    #[tokio::test]
    async fn test_ps_table_running_flag() {
        let fake = FakeRuntime::new(sample_containers());
        let args = PsArgs {
            all: false,
            running: true,
            json: false,
        };

        let output = execute(args, &fake).await.unwrap();
        assert!(!fake.all_passed.load(Ordering::SeqCst));
        assert!(output.contains("dev-postgres"));
        assert!(output.contains("postgres:16"));
        assert!(output.contains("Up 2 hours"));
        assert!(output.contains("0.0.0.0:5432->5432/tcp"));
        assert!(output.contains("2h"));
        // stopped container should be omitted when running: true
        assert!(!output.contains("dev-redis"));
    }

    #[tokio::test]
    async fn test_ps_table_all_flag() {
        let fake = FakeRuntime::new(sample_containers());
        let args = PsArgs {
            all: true,
            running: false,
            json: false,
        };

        let output = execute(args, &fake).await.unwrap();
        assert!(fake.all_passed.load(Ordering::SeqCst));
        assert!(output.contains("dev-postgres"));
        assert!(output.contains("dev-redis"));
        assert!(output.contains("Exited (0) 10 minutes ago"));
    }

    #[tokio::test]
    async fn test_ps_table_empty_list() {
        let fake = FakeRuntime::new(vec![]);
        let args = PsArgs::default();

        let output = execute(args, &fake).await.unwrap();
        assert!(output.contains("No containers found"));
        assert!(output.contains("asur up"));
    }

    #[tokio::test]
    async fn test_ps_json_output() {
        let fake = FakeRuntime::new(sample_containers());
        let args = PsArgs {
            json: true,
            ..Default::default()
        };

        let output = execute(args, &fake).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert!(parsed.is_array());
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["name"], "dev-postgres");
        assert_eq!(arr[0]["image"], "postgres:16");
        assert_eq!(arr[0]["state"], "running");
        assert_eq!(arr[0]["ports"][0], "0.0.0.0:5432->5432/tcp");
        assert_eq!(arr[1]["name"], "dev-redis");
        assert_eq!(arr[1]["state"], "exited");
    }

    #[tokio::test]
    async fn test_ps_json_empty_list() {
        let fake = FakeRuntime::new(vec![]);
        let args = PsArgs {
            json: true,
            ..Default::default()
        };

        let output = execute(args, &fake).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed, serde_json::json!([]));
    }
}
