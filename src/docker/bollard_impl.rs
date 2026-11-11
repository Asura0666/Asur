//! Bollard (Docker API) implementation of [`ContainerRuntime`].

use anyhow::{Context, Result};
use bollard::{
    Docker,
    container::{
        Config, CreateContainerOptions, ListContainersOptions, RemoveContainerOptions,
        StopContainerOptions,
    },
    image::CreateImageOptions,
    models::{
        ContainerSummary, HostConfig, Port, PortBinding, RestartPolicy, RestartPolicyNameEnum,
    },
};
use futures_util::StreamExt;
use std::collections::HashMap;

use super::{ContainerInfo, ContainerRuntime, CreateContainerConfig};

/// Docker backend.  Connects to the local Docker daemon via the platform
/// default socket / named pipe.
pub struct DockerRuntime {
    docker: Docker,
}

impl DockerRuntime {
    /// Connect to the Docker daemon using the default transport for the
    /// current platform (Unix socket on Linux/macOS, named pipe on Windows).
    ///
    /// Returns a human-readable error if the daemon is not reachable.
    pub fn connect() -> Result<Self> {
        let docker = Docker::connect_with_local_defaults().context(
            "could not connect to Docker — is the Docker daemon running?\n\
             \n\
             Try one of:\n\
             • macOS / Linux : `open -a Docker` or `sudo systemctl start docker`\n\
             • Windows       : start Docker Desktop from the system tray\n\
             • CI            : make sure the `docker` service is enabled in your workflow",
        )?;
        Ok(Self { docker })
    }
}

impl ContainerRuntime for DockerRuntime {
    async fn list_containers(&self, all: bool) -> Result<Vec<ContainerInfo>> {
        let options = ListContainersOptions::<String> {
            all,
            ..Default::default()
        };

        let summaries = self
            .docker
            .list_containers(Some(options))
            .await
            .map_err(format_docker_error)?;

        Ok(summaries.into_iter().map(container_info_from).collect())
    }

    async fn run_container(&self, config: CreateContainerConfig) -> Result<ContainerInfo> {
        // 1. Ensure image is available locally; pull if not
        if self.docker.inspect_image(&config.image).await.is_err() {
            let options = Some(CreateImageOptions {
                from_image: config.image.clone(),
                ..Default::default()
            });
            let mut stream = self.docker.create_image(options, None, None);
            while let Some(result) = stream.next().await {
                if let Err(e) = result {
                    return Err(format_docker_error(e));
                }
            }
        }

        // 2. Configure port bindings
        let mut port_bindings = HashMap::new();
        let mut port_strings = Vec::new();

        for &(host_port, container_port) in &config.ports {
            let port_key = format!("{container_port}/tcp");
            port_bindings.insert(
                port_key,
                Some(vec![PortBinding {
                    host_ip: Some("0.0.0.0".to_string()),
                    host_port: Some(host_port.to_string()),
                }]),
            );
            port_strings.push(format!("0.0.0.0:{host_port}->{container_port}/tcp"));
        }

        // 3. Configure volume bindings
        let binds: Option<Vec<String>> = if config.volumes.is_empty() {
            None
        } else {
            Some(
                config
                    .volumes
                    .iter()
                    .map(|(vol, path)| format!("{vol}:{path}"))
                    .collect(),
            )
        };

        // 4. Host config
        let host_config = HostConfig {
            port_bindings: Some(port_bindings),
            binds,
            restart_policy: Some(RestartPolicy {
                name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
                ..Default::default()
            }),
            ..Default::default()
        };

        // 5. Container config
        let container_config = Config {
            image: Some(config.image.clone()),
            env: if config.env.is_empty() {
                None
            } else {
                Some(config.env)
            },
            cmd: config.command,
            host_config: Some(host_config),
            ..Default::default()
        };

        let create_options = CreateContainerOptions {
            name: config.name.as_str(),
            platform: None,
        };

        self.docker
            .create_container(Some(create_options), container_config)
            .await
            .map_err(|e| {
                let s = e.to_string();
                if s.contains("Conflict") || s.contains("already in use") {
                    anyhow::anyhow!(
                        "container `{}` already exists.\n\n\
                         Run `asur ps` to check its status, or remove it with `docker rm -f {}`.",
                        config.name,
                        config.name
                    )
                } else {
                    format_docker_error(e)
                }
            })?;

        // 6. Start container
        if let Err(e) = self
            .docker
            .start_container::<String>(&config.name, None)
            .await
        {
            // Clean up the created container so it isn't left dangling in 'Created' state
            let _ = self.docker.remove_container(&config.name, None).await;

            let s = e.to_string();
            let lower = s.to_lowercase();
            if lower.contains("port is already allocated")
                || lower.contains("address already in use")
                || lower.contains("bind for")
            {
                return Err(anyhow::anyhow!(
                    "failed to start `{}`: port conflict.\n\n\
                     One or more requested ports are already in use by another container or process.\n\
                     Run `asur ps` to check existing containers, or specify a different port.",
                    config.name
                ));
            }
            return Err(format_docker_error(e));
        }

        Ok(ContainerInfo {
            name: config.name,
            image: config.image,
            status: "Up just now".to_string(),
            state: "running".to_string(),
            ports: port_strings,
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
        })
    }

    async fn stop_container(&self, id_or_name: &str) -> Result<()> {
        let options = StopContainerOptions { t: 10 };
        match self.docker.stop_container(id_or_name, Some(options)).await {
            Ok(_) => Ok(()),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => {
                // 304 means container is already stopped, which is expected
                Ok(())
            }
            Err(e) => Err(format_docker_error(e)),
        }
    }

    async fn remove_container(&self, id_or_name: &str, remove_volumes: bool) -> Result<()> {
        let options = RemoveContainerOptions {
            force: true,
            v: remove_volumes,
            ..Default::default()
        };
        self.docker
            .remove_container(id_or_name, Some(options))
            .await
            .map_err(format_docker_error)?;
        Ok(())
    }

    async fn stream_logs(
        &self,
        id_or_name: &str,
        tail: usize,
        follow: bool,
        tx: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Result<()> {
        use bollard::container::LogsOptions;
        let options = LogsOptions::<String> {
            stdout: true,
            stderr: true,
            follow,
            tail: tail.to_string(),
            ..Default::default()
        };
        let mut stream = self.docker.logs(id_or_name, Some(options));
        while let Some(msg) = stream.next().await {
            match msg {
                Ok(output) => {
                    let line = output.to_string();
                    if tx.send(line).is_err() {
                        break;
                    }
                }
                Err(e) => return Err(format_docker_error(e)),
            }
        }
        Ok(())
    }

    async fn get_clean_report(&self) -> Result<super::CleanReport> {
        let df = self.docker.df().await.map_err(format_docker_error)?;
        let mut stopped_containers = Vec::new();
        let mut dangling_images = Vec::new();
        let mut unused_volumes = Vec::new();

        if let Some(containers) = df.containers {
            for c in containers {
                let state = c.state.as_deref().unwrap_or_default();
                if state != "running" {
                    let id = c.id.unwrap_or_default();
                    let name = c
                        .names
                        .and_then(|n| n.into_iter().next())
                        .unwrap_or_default()
                        .trim_start_matches('/')
                        .to_string();
                    let size = c.size_rw.unwrap_or(0);
                    stopped_containers.push((id, name, size));
                }
            }
        }

        if let Some(images) = df.images {
            for img in images {
                let is_dangling = img.repo_tags.is_empty()
                    || img.repo_tags.iter().any(|tag| tag == "<none>:<none>");
                let container_count = img.containers;
                if is_dangling || container_count <= 0 {
                    let id = img.id;
                    let tag = img
                        .repo_tags
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "<none>".to_string());
                    let size = img.size;
                    dangling_images.push((id, tag, size));
                }
            }
        }

        if let Some(volumes) = df.volumes {
            for v in volumes {
                let ref_count = v.usage_data.as_ref().map(|u| u.ref_count).unwrap_or(0);
                if ref_count <= 0 {
                    let name = v.name;
                    let size = v.usage_data.as_ref().map(|u| u.size).unwrap_or(0);
                    unused_volumes.push((name, size));
                }
            }
        }

        Ok(super::CleanReport {
            stopped_containers,
            dangling_images,
            unused_volumes,
        })
    }

    async fn clean_resources(
        &self,
        container_ids: &[String],
        image_ids: &[String],
        volume_names: &[String],
    ) -> Result<super::CleanResults> {
        let mut results = super::CleanResults::default();

        for id in container_ids {
            let options = RemoveContainerOptions {
                force: true,
                v: false,
                ..Default::default()
            };
            if self
                .docker
                .remove_container(id, Some(options))
                .await
                .is_ok()
            {
                results.containers_removed += 1;
            }
        }

        for id in image_ids {
            let options = bollard::image::RemoveImageOptions {
                force: true,
                ..Default::default()
            };
            if self
                .docker
                .remove_image(id, Some(options), None)
                .await
                .is_ok()
            {
                results.images_removed += 1;
            }
        }

        for name in volume_names {
            if self.docker.remove_volume(name, None).await.is_ok() {
                results.volumes_removed += 1;
            }
        }

        results.space_reclaimed_bytes = 0;
        Ok(results)
    }

    async fn get_container_details(&self, id_or_name: &str) -> Result<super::ContainerDetails> {
        let inspect = self
            .docker
            .inspect_container(id_or_name, None)
            .await
            .map_err(format_docker_error)?;

        let name = inspect
            .name
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_string();
        let config = inspect.config.unwrap_or_default();
        let image = config.image.unwrap_or_default();
        let env = config.env.unwrap_or_default();

        Ok(super::ContainerDetails { name, image, env })
    }
}

/// Convert a Docker API error into a friendly, actionable message.
fn format_docker_error(err: bollard::errors::Error) -> anyhow::Error {
    let err_str = err.to_string();
    let lower = err_str.to_lowercase();

    // Check for port allocation errors
    if lower.contains("port is already allocated")
        || lower.contains("address already in use")
        || lower.contains("bind for")
    {
        return anyhow::anyhow!(
            "port conflict: {err_str}.\n\n\
             The port is already in use. Run `asur ps` to see active containers or choose another port."
        );
    }

    // Check for daemon connection issues
    if lower.contains("connection refused")
        || lower.contains("client error (connect)")
        || lower.contains("failed to connect")
        || lower.contains("error in the hyper legacy client")
        || lower.contains("no such file")
        || lower.contains("cannot find the file specified")
        || lower.contains("named pipe")
        || lower.contains("broken pipe")
        || lower.contains("os error 2")
        || lower.contains("os error 10061")
    {
        anyhow::anyhow!(
            "cannot connect to the Docker daemon. Is Docker running?\n\n\
             To start it:\n  \
             • Windows: start Docker Desktop from the Start menu or system tray\n  \
             • macOS:   open Docker Desktop (`open -a Docker`)\n  \
             • Linux:   run `sudo systemctl start docker`"
        )
    } else {
        anyhow::anyhow!("{err_str}")
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Convert a bollard [`ContainerSummary`] into our domain type.
fn container_info_from(s: ContainerSummary) -> ContainerInfo {
    let name = s
        .names
        .unwrap_or_default()
        .into_iter()
        .next()
        .unwrap_or_default()
        .trim_start_matches('/')
        .to_owned();

    let image = s.image.unwrap_or_default();
    let status = s.status.unwrap_or_default();
    let state = s.state.unwrap_or_default();
    let created = s.created.unwrap_or(0);

    let ports = format_ports(s.ports.unwrap_or_default());

    ContainerInfo {
        name,
        image,
        status,
        state,
        ports,
        created,
    }
}

/// Turn a list of bollard [`Port`] records into compact human-readable strings.
///
/// Each item in the returned `Vec` looks like `0.0.0.0:5432->5432/tcp`.
/// Ports that have no host binding are shown as `5432/tcp`.
fn format_ports(ports: Vec<Port>) -> Vec<String> {
    // De-duplicate: bollard sometimes emits one entry per IP.
    let mut seen: HashMap<String, ()> = HashMap::new();
    let mut result = Vec::new();

    for p in ports {
        let proto = p.typ.map(|t| format!("{t}")).unwrap_or_default();
        let container_port = p.private_port;

        let entry = match (p.ip.as_deref(), p.public_port) {
            // Has a host binding
            (Some(ip), Some(host_port)) => {
                // Collapse 0.0.0.0 and :: into a single entry
                let display_ip = if ip == "::" { "0.0.0.0" } else { ip };
                format!("{display_ip}:{host_port}->{container_port}/{proto}")
            }
            // No host binding
            _ => format!("{container_port}/{proto}"),
        };

        if seen.insert(entry.clone(), ()).is_none() {
            result.push(entry);
        }
    }

    result.sort(); // deterministic order for tests
    result
}

// ── unit tests (no daemon needed) ────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::PortTypeEnum;

    fn make_port(ip: Option<&str>, public: Option<u16>, private: u16, proto: PortTypeEnum) -> Port {
        Port {
            ip: ip.map(str::to_owned),
            public_port: public,
            private_port: private,
            typ: Some(proto),
        }
    }

    #[test]
    fn no_binding_shows_container_port_only() {
        let ports = vec![make_port(None, None, 5432, PortTypeEnum::TCP)];
        assert_eq!(format_ports(ports), vec!["5432/tcp"]);
    }

    #[test]
    fn bound_port_includes_host() {
        let ports = vec![make_port(
            Some("0.0.0.0"),
            Some(5432),
            5432,
            PortTypeEnum::TCP,
        )];
        assert_eq!(format_ports(ports), vec!["0.0.0.0:5432->5432/tcp"]);
    }

    #[test]
    fn ipv6_collapsed_to_ipv4_display() {
        let ports = vec![
            make_port(Some("0.0.0.0"), Some(5432), 5432, PortTypeEnum::TCP),
            make_port(Some("::"), Some(5432), 5432, PortTypeEnum::TCP),
        ];
        // Both collapse to 0.0.0.0, dedup removes the second.
        assert_eq!(format_ports(ports), vec!["0.0.0.0:5432->5432/tcp"]);
    }

    #[test]
    fn multiple_ports_sorted() {
        let ports = vec![
            make_port(Some("0.0.0.0"), Some(8080), 80, PortTypeEnum::TCP),
            make_port(Some("0.0.0.0"), Some(443), 443, PortTypeEnum::TCP),
        ];
        let result = format_ports(ports);
        assert_eq!(result[0], "0.0.0.0:443->443/tcp");
        assert_eq!(result[1], "0.0.0.0:8080->80/tcp");
    }

    /// Live-daemon test: skipped by default; run with:
    ///   cargo test -- --ignored live_docker
    #[tokio::test]
    #[ignore = "requires a live Docker daemon; run with --ignored"]
    async fn live_docker_list_containers() {
        let rt = DockerRuntime::connect().expect("Docker should be running");
        let containers = rt
            .list_containers(true)
            .await
            .expect("list_containers failed");
        // Just make sure the call succeeds; content is environment-specific.
        println!("Found {} container(s)", containers.len());
    }
}
