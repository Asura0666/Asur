//! Container runtime abstraction.
//!
//! [`ContainerRuntime`] is the trait that all backend implementations must satisfy.
//! Currently only [`DockerRuntime`] is provided; Podman support can be added later
//! without changing calling code.

mod bollard_impl;

pub use bollard_impl::DockerRuntime;

use anyhow::Result;
use std::future::Future;

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
    /// Port mappings: `(host_port, container_port)`.
    pub ports: Vec<(u16, u16)>,
    /// Named volume or host path bindings: `(volume_name, container_path)`.
    pub volumes: Vec<(String, String)>,
    /// Command override (e.g. `["redis-server", "--requirepass", "secret"]`).
    pub command: Option<Vec<String>>,
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

    /// Create and run a new container from the given configuration.
    /// Pulls the image if it is not present locally.
    fn run_container(
        &self,
        config: CreateContainerConfig,
    ) -> impl Future<Output = Result<ContainerInfo>> + Send;

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
