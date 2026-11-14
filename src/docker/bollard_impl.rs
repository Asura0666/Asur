//! Bollard (Docker API) implementation of [`ContainerRuntime`].

use anyhow::{Context, Result, bail};
use bollard::{
    Docker,
    auth::DockerCredentials,
    container::{
        Config, CreateContainerOptions, ListContainersOptions, RemoveContainerOptions,
        StopContainerOptions,
    },
    image::{CreateImageOptions, ListImagesOptions, SearchImagesOptions},
    models::{
        ContainerSummary, HostConfig, Mount as DockerMount, MountTypeEnum, Port, PortBinding,
        RestartPolicy, RestartPolicyNameEnum,
    },
};
use futures_util::StreamExt;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use super::{
    ContainerInfo, ContainerRuntime, CreateContainerConfig, DockerUnavailable, ImageMetadata,
    ImageReference, ImageSearchResult, ImageTag, LocalImage, MountKind, OperationEvent,
    OperationStatus, PortProtocol, PullPolicy, RestartPolicy as AsurRestartPolicy,
};

/// Docker backend.  Connects to the local Docker daemon via the platform
/// default socket / named pipe.
#[derive(Clone)]
pub struct DockerRuntime {
    docker: Docker,
}

impl DockerRuntime {
    /// Connect to the Docker daemon using the default transport for the
    /// current platform (Unix socket on Linux/macOS, named pipe on Windows).
    ///
    /// Returns a human-readable error if the daemon is not reachable.
    pub fn connect() -> Result<Self> {
        let docker = Docker::connect_with_local_defaults().map_err(|_| DockerUnavailable)?;
        Ok(Self { docker })
    }

    async fn pull_image(
        &self,
        reference: &ImageReference,
        events: &tokio::sync::mpsc::UnboundedSender<OperationEvent>,
    ) -> Result<()> {
        let _ = events.send(OperationEvent::stage(
            OperationStatus::Pulling,
            format!("pulling image {reference}"),
        ));
        let credentials = registry_credentials(reference)?;
        let result = self
            .pull_image_once(reference, events, credentials.clone())
            .await;
        if let Err(error) = result {
            if credentials.is_some()
                && reference.registry() == "docker.io"
                && is_authentication_error(&error)
            {
                let _ = events.send(OperationEvent::stage(
                    OperationStatus::Preparing,
                    "saved Docker Hub credentials were rejected; retrying this public pull anonymously",
                ));
                return self
                    .pull_image_once(reference, events, None)
                    .await
                    .map_err(|error| format_pull_error(error, reference));
            }
            return Err(format_pull_error(error, reference));
        }
        Ok(())
    }

    async fn pull_image_once(
        &self,
        reference: &ImageReference,
        events: &tokio::sync::mpsc::UnboundedSender<OperationEvent>,
        credentials: Option<DockerCredentials>,
    ) -> std::result::Result<(), bollard::errors::Error> {
        let options = Some(CreateImageOptions {
            from_image: reference.repository().to_string(),
            tag: reference.pull_tag().to_string(),
            ..Default::default()
        });
        let mut stream = self.docker.create_image(options, None, credentials);
        while let Some(result) = stream.next().await {
            match result {
                Ok(update) => {
                    let layer_id = update.id.clone();
                    let detail = match (update.id, update.status, update.progress) {
                        (Some(layer), Some(status), Some(progress)) => {
                            format!("{layer}: {status} ({progress})")
                        }
                        (Some(layer), Some(status), None) => format!("{layer}: {status}"),
                        (_, Some(status), _) => status,
                        _ => "waiting for Docker pull progress".to_string(),
                    };
                    let (current_bytes, total_bytes) = update
                        .progress_detail
                        .map(|d| (d.current, d.total))
                        .unwrap_or((None, None));
                    let _ = events.send(OperationEvent {
                        status: OperationStatus::Pulling,
                        detail,
                        layer_id,
                        current_bytes,
                        total_bytes,
                    });
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    async fn image_metadata(&self, reference: &str) -> Result<ImageMetadata> {
        let inspect = self
            .docker
            .inspect_image(reference)
            .await
            .map_err(format_docker_error)?;
        let config = inspect.config.unwrap_or_default();
        let mut exposed_ports: Vec<String> = config
            .exposed_ports
            .unwrap_or_default()
            .into_keys()
            .collect();
        exposed_ports.sort();
        let mut declared_volumes: Vec<String> =
            config.volumes.unwrap_or_default().into_keys().collect();
        declared_volumes.sort();

        Ok(ImageMetadata {
            reference: reference.to_string(),
            id: inspect.id.unwrap_or_default(),
            digests: inspect.repo_digests.unwrap_or_default(),
            labels: config
                .labels
                .unwrap_or_default()
                .into_iter()
                .collect::<BTreeMap<_, _>>(),
            exposed_ports,
            declared_volumes,
            entrypoint: config.entrypoint.unwrap_or_default(),
            command: config.cmd.unwrap_or_default(),
            architecture: inspect.architecture.unwrap_or_default(),
            os: inspect.os.unwrap_or_default(),
            size: inspect.size.unwrap_or_default(),
        })
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

    async fn search_images(&self, query: &str, limit: usize) -> Result<Vec<ImageSearchResult>> {
        let options = SearchImagesOptions::<String> {
            term: query.to_string(),
            limit: Some(limit as u64),
            ..Default::default()
        };
        let results = self
            .docker
            .search_images(options)
            .await
            .map_err(format_docker_error)?;
        Ok(results
            .into_iter()
            .map(|item| ImageSearchResult {
                source: "docker.io".to_string(),
                name: item.name.unwrap_or_default(),
                description: item.description.unwrap_or_default(),
                official: item.is_official,
                verified: None,
                pulls: None,
                tags: Vec::new(),
                size: None,
                architecture: None,
            })
            .collect())
    }

    async fn list_image_tags(&self, repository: &str, limit: usize) -> Result<Vec<ImageTag>> {
        let (namespace, repository) = docker_hub_repository_parts(repository)?;
        let first_url = format!(
            "https://hub.docker.com/v2/namespaces/{namespace}/repositories/{repository}/tags"
        );
        let client = reqwest::Client::builder()
            .user_agent(concat!("asur/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let wanted = limit.max(1);
        let mut next_url = Some(first_url);
        let mut first_request = true;
        let mut hub_tags = Vec::new();
        while let Some(url) = next_url.take() {
            let mut request = client.get(url);
            if first_request {
                request = request.query(&[("page_size", wanted.min(100))]);
                first_request = false;
            }
            let response = request
                .send()
                .await
                .context("could not contact Docker Hub to discover image tags")?
                .error_for_status()
                .context("Docker Hub could not return tags for this repository")?
                .json::<DockerHubTagsResponse>()
                .await
                .context("Docker Hub returned an invalid tag response")?;
            next_url = response.next;
            hub_tags.extend(response.results);
            if hub_tags.len() >= wanted {
                break;
            }
        }
        hub_tags.truncate(wanted);

        Ok(hub_tags
            .into_iter()
            .map(|tag| {
                let mut platforms: Vec<String> = tag
                    .images
                    .into_iter()
                    .filter_map(|image| match (image.os, image.architecture) {
                        (Some(os), Some(architecture)) => Some(format!("{os}/{architecture}")),
                        _ => None,
                    })
                    .collect();
                platforms.sort();
                platforms.dedup();
                ImageTag {
                    name: tag.name,
                    size: tag.full_size,
                    platforms,
                }
            })
            .collect())
    }

    async fn list_images(&self) -> Result<Vec<LocalImage>> {
        let options = ListImagesOptions::<String> {
            all: false,
            digests: true,
            ..Default::default()
        };
        let images = self
            .docker
            .list_images(Some(options))
            .await
            .map_err(format_docker_error)?;
        Ok(images
            .into_iter()
            .map(|image| LocalImage {
                id: image.id,
                tags: image.repo_tags,
                digests: image.repo_digests,
                size: image.size,
                created: image.created,
            })
            .collect())
    }

    async fn inspect_image(&self, reference: &str) -> Result<ImageMetadata> {
        self.image_metadata(reference).await
    }

    async fn prepare_image(
        &self,
        reference: &ImageReference,
        pull_policy: PullPolicy,
        events: tokio::sync::mpsc::UnboundedSender<OperationEvent>,
    ) -> Result<ImageMetadata> {
        let local = match self.docker.inspect_image(reference.as_str()).await {
            Ok(_) => true,
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => false,
            Err(error) => return Err(format_docker_error(error)),
        };
        match (pull_policy, local) {
            (PullPolicy::Never, false) => {
                bail!("image `{reference}` is not available locally and pull policy is `never`")
            }
            (PullPolicy::Always, _) | (PullPolicy::Missing, false) => {
                self.pull_image(reference, &events).await?;
            }
            (PullPolicy::Never | PullPolicy::Missing, true) => {}
        }
        self.image_metadata(reference.as_str()).await
    }

    async fn run_container(&self, config: CreateContainerConfig) -> Result<ContainerInfo> {
        let (events, _receiver) = tokio::sync::mpsc::unbounded_channel();
        self.run_container_with_progress(config, events).await
    }

    async fn run_container_with_progress(
        &self,
        config: CreateContainerConfig,
        events: tokio::sync::mpsc::UnboundedSender<OperationEvent>,
    ) -> Result<ContainerInfo> {
        let readiness_config = config.clone();
        let emit = |status, detail: String| {
            let _ = events.send(OperationEvent::stage(status, detail));
        };
        emit(
            OperationStatus::CheckingImage,
            format!("checking image {}", config.image),
        );
        // 1. Apply the requested pull policy using Docker's standard auth config.
        let image_reference = config.image.parse::<ImageReference>()?;
        if let Err(error) = self
            .prepare_image(&image_reference, config.pull_policy, events.clone())
            .await
        {
            emit(OperationStatus::Failed, error.to_string());
            return Err(error);
        }

        // 2. Configure port bindings
        let mut port_bindings = HashMap::new();
        let mut port_strings = Vec::new();

        for binding in &config.ports {
            let host_ip = binding
                .host_ip
                .clone()
                .unwrap_or_else(|| "0.0.0.0".to_string());
            port_bindings
                .entry(binding.container_key())
                .or_insert_with(|| Some(Vec::new()))
                .as_mut()
                .expect("port bindings are always a list")
                .push(PortBinding {
                    host_ip: Some(host_ip.clone()),
                    host_port: Some(binding.host_port.to_string()),
                });
            port_strings.push(format!(
                "{host_ip}:{}->{}/{}",
                binding.host_port, binding.container_port, binding.protocol
            ));
        }
        let exposed_ports = if port_bindings.is_empty() {
            None
        } else {
            Some(
                port_bindings
                    .keys()
                    .cloned()
                    .map(|key| (key, HashMap::new()))
                    .collect(),
            )
        };

        // 3. Configure volume bindings
        let mounts: Option<Vec<DockerMount>> = if config.mounts.is_empty() {
            None
        } else {
            Some(
                config
                    .mounts
                    .iter()
                    .map(|mount| DockerMount {
                        target: Some(mount.target.clone()),
                        source: Some(mount.source.clone()),
                        typ: Some(match mount.kind {
                            MountKind::Bind => MountTypeEnum::BIND,
                            MountKind::Volume => MountTypeEnum::VOLUME,
                        }),
                        read_only: Some(mount.read_only),
                        ..Default::default()
                    })
                    .collect(),
            )
        };

        // 4. Host config
        let host_config = HostConfig {
            port_bindings: Some(port_bindings),
            mounts,
            restart_policy: Some(to_docker_restart_policy(config.restart)),
            network_mode: config.network.clone(),
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
            exposed_ports,
            host_config: Some(host_config),
            ..Default::default()
        };

        let create_options = CreateContainerOptions {
            name: config.name.as_str(),
            platform: None,
        };

        emit(
            OperationStatus::Creating,
            format!("creating container {}", config.name),
        );
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
        emit(
            OperationStatus::Starting,
            format!("starting container {}", config.name),
        );
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
                let error = anyhow::anyhow!(
                    "failed to start `{}`: port conflict.\n\n\
                     One or more requested ports are already in use by another container or process.\n\
                     Run `asur ps` to check existing containers, or specify a different port.",
                    config.name
                );
                emit(OperationStatus::Failed, error.to_string());
                return Err(error);
            }
            let error = format_docker_error(e);
            emit(OperationStatus::Failed, error.to_string());
            return Err(error);
        }

        if readiness_config.probe_readiness {
            self.wait_for_readiness(&readiness_config, events.clone())
                .await?;
        }

        let info = ContainerInfo {
            name: config.name,
            image: config.image,
            status: "Up just now".to_string(),
            state: "running".to_string(),
            ports: port_strings,
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
        };
        emit(
            OperationStatus::Running,
            format!("{} is running", info.name),
        );
        Ok(info)
    }

    async fn wait_for_readiness(
        &self,
        config: &CreateContainerConfig,
        events: tokio::sync::mpsc::UnboundedSender<OperationEvent>,
    ) -> Result<()> {
        use tokio::net::TcpStream;
        use tokio::time::{Duration, Instant, sleep, timeout};

        // A listening mapped TCP port proves the service has bound its public
        // interface; unlike Docker's running state it is not assumed ready.
        // Services with no published port have no safe generic probe.
        let Some(host_port) = config
            .ports
            .iter()
            .find(|binding| binding.protocol == PortProtocol::Tcp)
            .map(|binding| binding.host_port)
        else {
            let _ = events.send(OperationEvent::stage(
                OperationStatus::WaitingForReadiness,
                "no published port; Docker started the container but readiness cannot be probed",
            ));
            return Ok(());
        };
        let _ = events.send(OperationEvent::stage(
            OperationStatus::WaitingForReadiness,
            format!("waiting for service to listen on 127.0.0.1:{host_port}"),
        ));
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match timeout(
                Duration::from_secs(1),
                TcpStream::connect(("127.0.0.1", host_port)),
            )
            .await
            {
                Ok(Ok(_)) => return Ok(()),
                _ if Instant::now() >= deadline => {
                    anyhow::bail!(
                        "container `{}` started but the service did not accept TCP connections on port {host_port} within 30 seconds",
                        config.name
                    );
                }
                _ => sleep(Duration::from_millis(250)).await,
            }
        }
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
            match self.docker.remove_container(id, Some(options)).await {
                Ok(_) => results.containers_removed += 1,
                Err(error) => results
                    .failures
                    .push(format!("container {id}: {}", format_docker_error(error))),
            }
        }

        for id in image_ids {
            let options = bollard::image::RemoveImageOptions {
                force: true,
                ..Default::default()
            };
            match self.docker.remove_image(id, Some(options), None).await {
                Ok(_) => results.images_removed += 1,
                Err(error) => results
                    .failures
                    .push(format!("image {id}: {}", format_docker_error(error))),
            }
        }

        for name in volume_names {
            match self.docker.remove_volume(name, None).await {
                Ok(_) => results.volumes_removed += 1,
                Err(error) => results
                    .failures
                    .push(format!("volume {name}: {}", format_docker_error(error))),
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

fn to_docker_restart_policy(policy: AsurRestartPolicy) -> RestartPolicy {
    let (name, maximum_retry_count) = match policy {
        AsurRestartPolicy::No => (RestartPolicyNameEnum::NO, None),
        AsurRestartPolicy::Always => (RestartPolicyNameEnum::ALWAYS, None),
        AsurRestartPolicy::UnlessStopped => (RestartPolicyNameEnum::UNLESS_STOPPED, None),
        AsurRestartPolicy::OnFailure {
            maximum_retry_count,
        } => (
            RestartPolicyNameEnum::ON_FAILURE,
            Some(maximum_retry_count.min(i64::MAX as u64) as i64),
        ),
    };
    RestartPolicy {
        name: Some(name),
        maximum_retry_count,
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DockerConfigFile {
    #[serde(default)]
    auths: HashMap<String, DockerAuthEntry>,
    creds_store: Option<String>,
    #[serde(default)]
    cred_helpers: HashMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
struct DockerAuthEntry {
    auth: Option<String>,
    username: Option<String>,
    password: Option<String>,
    #[serde(rename = "identitytoken")]
    identity_token_lower: Option<String>,
    #[serde(rename = "identityToken")]
    identity_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct CredentialHelperResponse {
    username: String,
    secret: String,
    #[serde(rename = "ServerURL")]
    server_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DockerHubTagsResponse {
    #[serde(default)]
    next: Option<String>,
    #[serde(default)]
    results: Vec<DockerHubTag>,
}

#[derive(Debug, Deserialize)]
struct DockerHubTag {
    name: String,
    full_size: Option<i64>,
    #[serde(default)]
    images: Vec<DockerHubTagImage>,
}

#[derive(Debug, Deserialize)]
struct DockerHubTagImage {
    architecture: Option<String>,
    os: Option<String>,
}

fn docker_hub_repository_parts(value: &str) -> Result<(String, String)> {
    let reference: ImageReference = value.parse()?;
    if reference.registry() != "docker.io" {
        bail!(
            "tag discovery currently supports Docker Hub only; `{}` belongs to `{}`",
            reference.repository(),
            reference.registry()
        );
    }

    let mut repository = reference.repository();
    if let Some(stripped) = repository
        .strip_prefix("docker.io/")
        .or_else(|| repository.strip_prefix("index.docker.io/"))
        .or_else(|| repository.strip_prefix("registry-1.docker.io/"))
    {
        repository = stripped;
    }
    let (namespace, name) = repository
        .split_once('/')
        .map_or(("library", repository), |(namespace, name)| {
            (namespace, name)
        });
    if name.contains('/') {
        bail!("invalid Docker Hub repository `{value}`");
    }
    Ok((namespace.to_string(), name.to_string()))
}

fn registry_credentials(reference: &ImageReference) -> Result<Option<DockerCredentials>> {
    let Some(path) = docker_config_path() else {
        return Ok(None);
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("could not read Docker config `{}`", path.display()));
        }
    };
    let config: DockerConfigFile = serde_json::from_str(&content)
        .with_context(|| format!("Docker config `{}` is invalid JSON", path.display()))?;
    let registry = reference.registry();
    let server = credential_server(registry);

    let helper = config
        .cred_helpers
        .iter()
        .find(|(key, _)| canonical_registry(key) == registry)
        .map(|(_, helper)| helper)
        .or(config.creds_store.as_ref());
    if let Some(helper) = helper {
        return credential_from_helper(helper, &server);
    }

    let entry = config
        .auths
        .iter()
        .find(|(key, _)| canonical_registry(key) == registry)
        .map(|(_, entry)| entry);
    Ok(entry.map(|entry| DockerCredentials {
        username: entry.username.clone(),
        password: entry.password.clone(),
        auth: entry.auth.clone(),
        serveraddress: Some(server),
        identitytoken: entry
            .identity_token
            .clone()
            .or_else(|| entry.identity_token_lower.clone()),
        ..Default::default()
    }))
}

fn docker_config_path() -> Option<PathBuf> {
    if let Some(directory) = std::env::var_os("DOCKER_CONFIG") {
        return Some(PathBuf::from(directory).join("config.json"));
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".docker").join("config.json"))
}

fn canonical_registry(value: &str) -> &str {
    let value = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
        .unwrap_or(value);
    let host = value.split('/').next().unwrap_or(value);
    match host {
        "index.docker.io" | "registry-1.docker.io" => "docker.io",
        other => other,
    }
}

fn credential_server(registry: &str) -> String {
    if registry == "docker.io" {
        "https://index.docker.io/v1/".to_string()
    } else {
        registry.to_string()
    }
}

fn credential_from_helper(helper: &str, server: &str) -> Result<Option<DockerCredentials>> {
    let executable = format!("docker-credential-{helper}");
    let mut child = Command::new(&executable)
        .arg("get")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| {
            format!(
                "Docker credential helper `{executable}` is configured but could not be started"
            )
        })?;
    if let Some(stdin) = &mut child.stdin {
        stdin.write_all(server.as_bytes())?;
        stdin.write_all(b"\n")?;
    }
    let output = child
        .wait_with_output()
        .context("Docker credential helper did not complete")?;
    if !output.status.success() {
        return Ok(None);
    }
    let response: CredentialHelperResponse = serde_json::from_slice(&output.stdout)
        .context("Docker credential helper returned an invalid response")?;
    Ok(Some(DockerCredentials {
        username: Some(response.username),
        password: Some(response.secret),
        serveraddress: response.server_url.or_else(|| Some(server.to_string())),
        ..Default::default()
    }))
}

fn format_pull_error(error: bollard::errors::Error, reference: &ImageReference) -> anyhow::Error {
    let message = error.to_string();
    let lower = message.to_ascii_lowercase();
    if is_authentication_error(&error) {
        let login = if reference.registry() == "docker.io" {
            "docker login".to_string()
        } else {
            format!("docker login {}", reference.registry())
        };
        anyhow::anyhow!(
            "registry authentication failed for `{}`. Run `{login}` and retry. Asur uses Docker's standard credential store and does not retain registry tokens",
            reference.registry()
        )
    } else if lower.contains("not found") || lower.contains("manifest unknown") {
        if reference.registry() == "docker.io" && reference.uses_latest() {
            if reference.repository().contains('/') {
                anyhow::anyhow!(
                    "image `{reference}` does not publish a `latest` tag. Choose an available tag with `asur image tags {}`",
                    reference.repository()
                )
            } else {
                anyhow::anyhow!(
                    "image `{reference}` was not found. A bare Docker Hub name resolves to `library/{}`; use `asur image search {}` to find the intended repository",
                    reference.repository(),
                    reference.repository()
                )
            }
        } else {
            anyhow::anyhow!(
                "image `{reference}` was not found. Check the repository, tag, and registry access"
            )
        }
    } else {
        anyhow::anyhow!("image pull failed for `{reference}`: {message}")
    }
}

fn is_authentication_error(error: &bollard::errors::Error) -> bool {
    let lower = error.to_string().to_ascii_lowercase();
    lower.contains("unauthorized")
        || lower.contains("authentication required")
        || lower.contains("denied")
        || lower.contains("no basic auth credentials")
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
        DockerUnavailable.into()
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

    #[test]
    fn docker_hub_repository_names_map_to_namespace_and_name() {
        assert_eq!(
            docker_hub_repository_parts("redis").unwrap(),
            ("library".to_string(), "redis".to_string())
        );
        assert_eq!(
            docker_hub_repository_parts("typesense/typesense:30.0").unwrap(),
            ("typesense".to_string(), "typesense".to_string())
        );
        assert_eq!(
            docker_hub_repository_parts("docker.io/typesense/typesense").unwrap(),
            ("typesense".to_string(), "typesense".to_string())
        );
        assert!(docker_hub_repository_parts("ghcr.io/acme/api").is_err());
    }

    #[test]
    fn docker_hub_tag_response_accepts_missing_platform_details() {
        let response: DockerHubTagsResponse = serde_json::from_str(
            r#"{"next":null,"results":[{"name":"30.0","full_size":123,"images":[]},{"name":"29.0","full_size":null}]}"#,
        )
        .unwrap();
        assert_eq!(response.results.len(), 2);
        assert_eq!(response.results[0].name, "30.0");
        assert!(response.results[1].images.is_empty());
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
