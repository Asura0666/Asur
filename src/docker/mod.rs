//! Container runtime abstraction.
//!
//! [`ContainerRuntime`] is the trait that all backend implementations must satisfy.
//! Currently only [`DockerRuntime`] is provided; Podman support can be added later
//! without changing calling code.

mod bollard_impl;
mod types;

pub use bollard_impl::DockerRuntime;
pub use types::{
    ImageReference, Mount, MountKind, PortBinding, PortProtocol, PullPolicy, RestartPolicy,
};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::future::Future;
use std::time::SystemTime;

#[derive(Debug, thiserror::Error)]
#[error(
    "Docker is not running. Start it and try again.\n\n  Windows: open Docker Desktop from the Start menu or system tray\n  macOS:   open Docker Desktop\n  Linux:   run `sudo systemctl start docker`"
)]
pub struct DockerUnavailable;

/// Lifecycle update emitted by a long-running Docker operation.  Presentation
/// code consumes these updates; runtimes never write directly to the terminal.
#[derive(Debug, Clone)]
pub struct OperationEvent {
    pub status: OperationStatus,
    pub detail: String,
    /// Docker pull-layer identifier, when the event belongs to a layer.
    pub layer_id: Option<String>,
    pub current_bytes: Option<i64>,
    pub total_bytes: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationStatus {
    Queued,
    Preparing,
    CheckingImage,
    Pulling,
    Creating,
    Starting,
    WaitingForReadiness,
    Running,
    Failed,
    #[expect(
        dead_code,
        reason = "Docker create/pull cancellation is unsafe until cleanup semantics are implemented"
    )]
    Cancelling,
    #[expect(
        dead_code,
        reason = "Docker create/pull cancellation is unsafe until cleanup semantics are implemented"
    )]
    Cancelled,
}

impl OperationEvent {
    pub fn stage(status: OperationStatus, detail: impl Into<String>) -> Self {
        Self {
            status,
            detail: detail.into(),
            layer_id: None,
            current_bytes: None,
            total_bytes: None,
        }
    }
}

/// In-memory record for an operation.  Operations deliberately live only for
/// this CLI process; Tokio tasks do not survive process exit.
#[derive(Debug, Clone)]
pub struct OperationRecord {
    pub id: u64,
    pub service: String,
    pub resource: String,
    pub image: String,
    pub created_at: SystemTime,
    pub status: OperationStatus,
    pub detail: String,
    pub error: Option<String>,
}

/// A single container as reported by the runtime.
#[derive(Debug, Clone)]
pub struct ContainerInfo {
    /// Container name (without the leading `/`).
    pub name: String,
    /// Full image reference, e.g. `postgres:16`.
    pub image: String,
    /// Raw status string from the daemon, e.g. `"Up 3 hours"` or `"Exited (0) 2 days ago"`.
    pub status: String,
    /// Parsed container state: `"running"`, `"exited"`, `"paused"`, etc.
    pub state: String,
    /// Human-readable port bindings, e.g. `["0.0.0.0:5432->5432/tcp"]`.
    pub ports: Vec<String>,
    /// Unix timestamp (seconds) when the container was created.
    pub created: i64,
}

/// Parameters for creating and starting a container.
#[derive(Debug, Clone)]
pub struct CreateContainerConfig {
    /// Container name (must be unique).
    pub name: String,
    /// Full image reference with tag, e.g. `postgres:16-alpine`.
    pub image: String,
    /// Environment variables in `"KEY=VALUE"` format.
    pub env: Vec<String>,
    /// Explicit host-to-container port mappings.
    pub ports: Vec<PortBinding>,
    /// Named-volume or bind mounts.
    pub mounts: Vec<Mount>,
    /// Docker network name or network mode.
    pub network: Option<String>,
    /// Container restart behavior.
    pub restart: RestartPolicy,
    /// Whether the runtime may pull the image.
    pub pull_policy: PullPolicy,
    /// Whether to probe the first published TCP port for readiness. Generic
    /// images leave this disabled because Asur cannot assume their semantics.
    pub probe_readiness: bool,
    /// Command override (e.g. `["redis-server", "--requirepass", "secret"]`).
    pub command: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageMetadata {
    pub reference: String,
    pub id: String,
    pub digests: Vec<String>,
    pub labels: BTreeMap<String, String>,
    pub exposed_ports: Vec<String>,
    pub declared_volumes: Vec<String>,
    pub entrypoint: Vec<String>,
    pub command: Vec<String>,
    pub architecture: String,
    pub os: String,
    pub size: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalImage {
    pub id: String,
    pub tags: Vec<String>,
    pub digests: Vec<String>,
    pub size: i64,
    pub created: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageSearchResult {
    pub source: String,
    pub name: String,
    pub description: String,
    pub official: Option<bool>,
    pub verified: Option<bool>,
    pub pulls: Option<u64>,
    pub tags: Vec<String>,
    pub size: Option<i64>,
    pub architecture: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageTag {
    pub name: String,
    pub size: Option<i64>,
    pub platforms: Vec<String>,
}

/// Summary of unused resources that can be cleaned.
#[derive(Debug, Clone, Default)]
pub struct CleanReport {
    pub stopped_containers: Vec<(String, String, i64)>, // (id, name, size_bytes)
    pub dangling_images: Vec<(String, String, i64)>,    // (id, tag/name, size_bytes)
    pub unused_volumes: Vec<(String, i64)>,             // (name, size_bytes)
}

/// Results of a clean operation.
#[derive(Debug, Clone, Default)]
pub struct CleanResults {
    pub containers_removed: usize,
    pub images_removed: usize,
    pub volumes_removed: usize,
    pub space_reclaimed_bytes: i64,
    /// Per-resource failures are retained so clean never reports a false
    /// success when the Docker daemon rejects an individual deletion.
    pub failures: Vec<String>,
}

/// Detailed information about a container.
#[derive(Debug, Clone, Default)]
pub struct ContainerDetails {
    pub name: String,
    pub image: String,
    pub env: Vec<String>,
}

/// Abstraction over a container runtime (Docker, Podman, …).
pub trait ContainerRuntime: Send + Sync {
    /// List containers. When `all` is `true` stopped containers are included.
    fn list_containers(&self, all: bool)
    -> impl Future<Output = Result<Vec<ContainerInfo>>> + Send;

    /// Search the runtime's configured public registry provider.
    fn search_images(
        &self,
        _query: &str,
        _limit: usize,
    ) -> impl Future<Output = Result<Vec<ImageSearchResult>>> + Send {
        async { anyhow::bail!("this container runtime does not support image search") }
    }

    /// List tags published for a Docker Hub repository.
    fn list_image_tags(
        &self,
        _repository: &str,
        _limit: usize,
    ) -> impl Future<Output = Result<Vec<ImageTag>>> + Send {
        async { anyhow::bail!("this container runtime does not support image tag discovery") }
    }

    /// List images stored by the local runtime.
    fn list_images(&self) -> impl Future<Output = Result<Vec<LocalImage>>> + Send {
        async { anyhow::bail!("this container runtime does not support listing images") }
    }

    /// Inspect an image already present in the local runtime.
    fn inspect_image(
        &self,
        _reference: &str,
    ) -> impl Future<Output = Result<ImageMetadata>> + Send {
        async { anyhow::bail!("this container runtime does not support image inspection") }
    }

    /// Apply a pull policy and return immutable/local image metadata.
    fn prepare_image(
        &self,
        _reference: &ImageReference,
        _pull_policy: PullPolicy,
        _events: tokio::sync::mpsc::UnboundedSender<OperationEvent>,
    ) -> impl Future<Output = Result<ImageMetadata>> + Send {
        async { anyhow::bail!("this container runtime does not support image pulls") }
    }

    /// Create and run a new container from the given configuration.
    /// Pulls the image if it is not present locally.
    fn run_container(
        &self,
        config: CreateContainerConfig,
    ) -> impl Future<Output = Result<ContainerInfo>> + Send;

    /// Run a container while publishing structured state/progress updates.
    /// Implementations that cannot expose finer-grained progress still provide
    /// accurate lifecycle states instead of pretending to know a percentage.
    fn run_container_with_progress(
        &self,
        config: CreateContainerConfig,
        events: tokio::sync::mpsc::UnboundedSender<OperationEvent>,
    ) -> impl Future<Output = Result<ContainerInfo>> + Send {
        async move {
            let _ = events.send(OperationEvent::stage(
                OperationStatus::Preparing,
                "checking image",
            ));
            let result = self.run_container(config).await;
            let event = match &result {
                Ok(_) => OperationEvent::stage(OperationStatus::Running, "container is running"),
                Err(error) => OperationEvent::stage(OperationStatus::Failed, error.to_string()),
            };
            let _ = events.send(event);
            result
        }
    }

    /// Wait until the service is reachable after Docker has started it.  A
    /// runtime may provide a protocol-specific probe; the default preserves
    /// compatibility for alternate runtimes and test doubles.
    fn wait_for_readiness(
        &self,
        _config: &CreateContainerConfig,
        events: tokio::sync::mpsc::UnboundedSender<OperationEvent>,
    ) -> impl Future<Output = Result<()>> + Send {
        async move {
            let _ = events.send(OperationEvent::stage(
                OperationStatus::WaitingForReadiness,
                "runtime does not expose a readiness probe",
            ));
            Ok(())
        }
    }

    /// Stop a running container by name or ID.
    fn stop_container(&self, id_or_name: &str) -> impl Future<Output = Result<()>> + Send;

    /// Remove a container by name or ID.
    fn remove_container(
        &self,
        id_or_name: &str,
        remove_volumes: bool,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Stream logs for a container.
    fn stream_logs(
        &self,
        id_or_name: &str,
        tail: usize,
        follow: bool,
        tx: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Get report of cleanable resources.
    fn get_clean_report(&self) -> impl Future<Output = Result<CleanReport>> + Send;

    /// Remove selected resources.
    fn clean_resources(
        &self,
        container_ids: &[String],
        image_ids: &[String],
        volume_names: &[String],
    ) -> impl Future<Output = Result<CleanResults>> + Send;

    /// Get container details including environment variables and image.
    fn get_container_details(
        &self,
        id_or_name: &str,
    ) -> impl Future<Output = Result<ContainerDetails>> + Send;
}
