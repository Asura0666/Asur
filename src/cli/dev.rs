//! Run and manage the reproducible stack in `.asur.toml`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde::Serialize;
use tokio::task::JoinSet;

use super::up::{CustomOptions, build_container_config};
use crate::catalog::{Catalog, Service};
use crate::docker::{
    ContainerHealthCheck, ContainerRuntime, CreateContainerConfig, DockerRuntime, ImageReference,
    PullPolicy, RestartPolicy,
};
use crate::project::{
    HealthCheck, ProjectManifest, ResolvedEnvironment, ServiceConfig, find_manifest,
};
use crate::ui;

#[derive(Args, Debug, Clone)]
pub struct DevArgs {
    #[command(subcommand)]
    pub command: DevCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum DevCommand {
    /// Start the project stack and wait for readiness
    Up {
        /// Start one profile and its dependencies
        #[arg(long)]
        profile: Option<String>,
    },
    /// Show the state of every project service
    Status {
        /// Show one profile and its dependencies
        #[arg(long)]
        profile: Option<String>,
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// View logs for one project service
    Logs {
        /// Service name (omit to choose interactively)
        service: Option<String>,
        /// Number of lines to show from the end
        #[arg(short = 't', long, default_value_t = 100)]
        tail: usize,
        /// Keep streaming new log lines
        #[arg(short, long)]
        follow: bool,
    },
    /// Stop and remove the project stack
    Down {
        /// Stop one profile and its dependencies
        #[arg(long)]
        profile: Option<String>,
        /// Also remove named and anonymous volumes attached to containers
        #[arg(long)]
        volumes: bool,
        /// Skip the interactive confirmation
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Export the manifest as a secret-safe Compose file
    Export {
        /// Destination Compose file
        #[arg(short, long, default_value = "compose.asur.yaml")]
        output: PathBuf,
        /// Export one profile and its dependencies
        #[arg(long)]
        profile: Option<String>,
        /// Replace an existing destination
        #[arg(long)]
        force: bool,
    },
}

pub async fn run(args: DevArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("could not determine the current directory")?;
    let (manifest_path, manifest) = find_manifest(&cwd)?;
    let root = manifest_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("project manifest has no parent directory"))?;
    match args.command {
        DevCommand::Up { profile } => up(&manifest, root, profile.as_deref()).await,
        DevCommand::Status { profile, json } => status(&manifest, profile.as_deref(), json).await,
        DevCommand::Logs {
            service,
            tail,
            follow,
        } => logs(&manifest, service, tail, follow).await,
        DevCommand::Down {
            profile,
            volumes,
            yes,
        } => down(&manifest, profile.as_deref(), volumes, yes).await,
        DevCommand::Export {
            output,
            profile,
            force,
        } => export(&manifest, root, &output, profile.as_deref(), force),
    }
}

async fn up(manifest: &ProjectManifest, root: &Path, profile: Option<&str>) -> Result<()> {
    let runtime = DockerRuntime::connect()?;
    let catalog = Catalog::load()?;
    let environment = ResolvedEnvironment::load(root, &manifest.project.env_files)?;
    let waves = manifest.startup_waves(profile)?;
    let mut existing = runtime
        .list_containers(true)
        .await?
        .into_iter()
        .map(|container| (container.name.clone(), container))
        .collect::<HashMap<_, _>>();

    for wave in waves {
        let mut starts = JoinSet::new();
        for service_name in wave {
            let container_name = manifest.container_name(&service_name);
            if let Some(container) = existing.get(&container_name) {
                if container.state == "running" {
                    ui::success(&format!(
                        "{service_name} is already running ({container_name})."
                    ));
                    continue;
                }
                runtime.remove_container(&container_name, false).await?;
                existing.remove(&container_name);
            }

            let config = build_service_config(manifest, &catalog, &environment, &service_name)?;
            if config.image.parse::<ImageReference>()?.uses_latest() {
                ui::warn(&format!(
                    "service `{service_name}` uses mutable image `{}`; pin a version or digest for reproducible environments",
                    config.image
                ));
            }
            ui::info(&format!("Starting {service_name} ({container_name})..."));
            let task_runtime = runtime.clone();
            starts.spawn(async move {
                let result = task_runtime.run_container(config).await;
                (service_name, result)
            });
        }

        let mut failures = Vec::new();
        while let Some(joined) = starts.join_next().await {
            match joined {
                Ok((service, Ok(container))) => {
                    ui::success(&format!("{service} is ready ({}).", container.name));
                    existing.insert(container.name.clone(), container);
                }
                Ok((service, Err(error))) => {
                    ui::error(&format!("{service}: {error:#}"));
                    failures.push(service);
                }
                Err(error) => failures.push(format!("startup task: {error}")),
            }
        }
        if !failures.is_empty() {
            bail!(
                "could not start {}; dependent services were not started",
                failures.join(", ")
            );
        }
    }
    ui::success(&format!("Project `{}` is ready.", manifest.project.name));
    Ok(())
}

fn build_service_config(
    manifest: &ProjectManifest,
    catalog: &Catalog,
    environment: &ResolvedEnvironment,
    service_name: &str,
) -> Result<CreateContainerConfig> {
    let service = &manifest.services[service_name];
    let resolved = environment.resolve_service(service_name, &service.environment)?;
    let mut config = if let Some(preset) = &service.preset {
        let catalog_service = catalog.find(preset).ok_or_else(|| {
            anyhow::anyhow!("service `{service_name}` uses unknown preset `{preset}`")
        })?;
        let port = service
            .ports
            .iter()
            .find(|binding| binding.container_port == catalog_service.default_port)
            .map(|binding| binding.host_port);
        let (config, _) = build_container_config(
            catalog_service,
            &CustomOptions {
                container_name: Some(manifest.container_name(service_name)),
                port,
                ..Default::default()
            },
        );
        config
    } else {
        CreateContainerConfig {
            name: manifest.container_name(service_name),
            image: service.image.clone().expect("validated image service"),
            env: Vec::new(),
            ports: Vec::new(),
            mounts: Vec::new(),
            network: None,
            restart: RestartPolicy::UnlessStopped,
            pull_policy: PullPolicy::Missing,
            probe_readiness: false,
            healthcheck: None,
            readiness_timeout_seconds: 30,
            command: None,
        }
    };

    merge_environment(&mut config.env, resolved);
    if !service.ports.is_empty() {
        config.ports.clone_from(&service.ports);
    }
    if !service.mounts.is_empty() {
        config.mounts.clone_from(&service.mounts);
    }
    if let Some(network) = &service.network {
        config.network = Some(network.clone());
    }
    if let Some(restart) = service.restart {
        config.restart = restart;
    }
    if let Some(pull) = service.pull {
        config.pull_policy = pull;
    }
    if !service.command.is_empty() {
        config.command = Some(service.command.clone());
    }
    match &service.health {
        Some(HealthCheck::Tcp {
            port,
            timeout_seconds,
        }) => {
            config.probe_readiness = true;
            config.readiness_timeout_seconds = *timeout_seconds;
            if let Some(index) = config
                .ports
                .iter()
                .position(|binding| binding.container_port == *port)
            {
                config.ports.swap(0, index);
            }
        }
        Some(HealthCheck::Command {
            command,
            interval_seconds,
            timeout_seconds,
            retries,
            start_period_seconds,
        }) => {
            config.probe_readiness = false;
            config.healthcheck = Some(ContainerHealthCheck {
                command: command.clone(),
                interval_seconds: *interval_seconds,
                timeout_seconds: *timeout_seconds,
                retries: *retries,
                start_period_seconds: *start_period_seconds,
            });
        }
        None => {}
    }
    Ok(config)
}

fn merge_environment(existing: &mut Vec<String>, overrides: Vec<String>) {
    let override_keys: BTreeSet<&str> = overrides
        .iter()
        .filter_map(|entry| entry.split_once('=').map(|(key, _)| key))
        .collect();
    existing.retain(|entry| {
        entry
            .split_once('=')
            .is_none_or(|(key, _)| !override_keys.contains(key))
    });
    existing.extend(overrides);
}

async fn status(manifest: &ProjectManifest, profile: Option<&str>, json: bool) -> Result<()> {
    let runtime = DockerRuntime::connect()?;
    let selected = selected_services(manifest, profile)?;
    let containers = runtime
        .list_containers(true)
        .await?
        .into_iter()
        .map(|container| (container.name.clone(), container))
        .collect::<HashMap<_, _>>();
    let rows = selected
        .into_iter()
        .map(|service| {
            let container_name = manifest.container_name(&service);
            let container = containers.get(&container_name);
            StatusRow {
                service,
                container: container_name,
                state: container
                    .map_or("not created", |value| value.state.as_str())
                    .to_string(),
                image: container
                    .map_or("—", |value| value.image.as_str())
                    .to_string(),
                ports: container.map_or_else(Vec::new, |value| value.ports.clone()),
            }
        })
        .collect::<Vec<_>>();
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    println!("SERVICE             STATE          CONTAINER                       PORTS");
    for row in rows {
        let ports = if row.ports.is_empty() {
            "—".to_string()
        } else {
            row.ports.join(", ")
        };
        println!(
            "{:<19} {:<14} {:<31} {}",
            row.service, row.state, row.container, ports
        );
    }
    Ok(())
}

#[derive(Serialize)]
struct StatusRow {
    service: String,
    container: String,
    state: String,
    image: String,
    ports: Vec<String>,
}

async fn logs(
    manifest: &ProjectManifest,
    service: Option<String>,
    tail: usize,
    follow: bool,
) -> Result<()> {
    let service = match service {
        Some(service) => {
            if !manifest.services.contains_key(&service) {
                bail!("unknown project service `{service}`");
            }
            service
        }
        None if std::io::stdin().is_terminal() => {
            let options = manifest.services.keys().cloned().collect::<Vec<_>>();
            inquire::Select::new("Choose a project service:", options)
                .with_help_message("Esc: cancel and exit")
                .prompt()
                .map_err(super::prompt_error)?
        }
        None => bail!("give a service name, for example `asur dev logs api`"),
    };
    super::logs::run(super::logs::LogsArgs {
        container: Some(manifest.container_name(&service)),
        tail,
        follow,
    })
    .await
}

async fn down(
    manifest: &ProjectManifest,
    profile: Option<&str>,
    volumes: bool,
    yes: bool,
) -> Result<()> {
    let runtime = DockerRuntime::connect()?;
    let mut services = selected_services(manifest, profile)?;
    services.reverse();
    let existing = runtime
        .list_containers(true)
        .await?
        .into_iter()
        .map(|container| (container.name.clone(), container))
        .collect::<HashMap<_, _>>();
    services.retain(|service| existing.contains_key(&manifest.container_name(service)));
    if services.is_empty() {
        ui::info("No project containers exist.");
        return Ok(());
    }
    if !yes {
        if !std::io::stdin().is_terminal() {
            bail!("non-interactive removal requires `--yes`");
        }
        let suffix = if volumes { " and their volumes" } else { "" };
        let confirmed = inquire::Confirm::new(&format!(
            "Stop and remove {} project container(s){suffix}?",
            services.len()
        ))
        .with_default(false)
        .with_help_message("Esc: cancel and exit")
        .prompt()
        .map_err(super::prompt_error)?;
        if !confirmed {
            return Err(super::UserCancelled.into());
        }
    }
    for service in services {
        let name = manifest.container_name(&service);
        let container = &existing[&name];
        if container.state == "running" || container.state == "paused" {
            ui::info(&format!("Stopping {service} ({name})..."));
            runtime.stop_container(&name).await?;
        }
        runtime.remove_container(&name, volumes).await?;
        ui::success(&format!("Removed {service} ({name})."));
    }
    Ok(())
}

fn selected_services(manifest: &ProjectManifest, profile: Option<&str>) -> Result<Vec<String>> {
    Ok(manifest
        .startup_waves(profile)?
        .into_iter()
        .flatten()
        .collect())
}

fn export(
    manifest: &ProjectManifest,
    root: &Path,
    output: &Path,
    profile: Option<&str>,
    force: bool,
) -> Result<()> {
    let output = if output.is_absolute() {
        output.to_path_buf()
    } else {
        root.join(output)
    };
    if output.exists() && !force {
        bail!(
            "`{}` already exists; use `--force` to replace it",
            output.display()
        );
    }
    let yaml = compose_yaml(manifest, profile)?;
    std::fs::write(&output, yaml)
        .with_context(|| format!("could not write `{}`", output.display()))?;
    ui::success(&format!("Exported `{}`.", output.display()));
    ui::hint(
        "Secret values were not exported; Compose will read referenced keys from your environment.",
    );
    Ok(())
}

#[derive(Serialize)]
struct ComposeDocument {
    services: BTreeMap<String, ComposeService>,
}

#[derive(Default, Serialize)]
struct ComposeService {
    image: String,
    container_name: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ports: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    volumes: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    environment: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    depends_on: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    network_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    restart: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    healthcheck: Option<ComposeHealthcheck>,
}

#[derive(Serialize)]
struct ComposeHealthcheck {
    test: Vec<String>,
    interval: String,
    timeout: String,
    retries: u64,
    start_period: String,
}

fn compose_yaml(manifest: &ProjectManifest, profile: Option<&str>) -> Result<String> {
    let catalog = Catalog::load()?;
    let selected: BTreeSet<_> = selected_services(manifest, profile)?.into_iter().collect();
    let mut services = BTreeMap::new();
    for name in &selected {
        let service = &manifest.services[name];
        let image = service_image(service, &catalog, name)?;
        let mut environment: BTreeMap<String, String> = service
            .environment
            .iter()
            .map(|(container_key, source_key)| {
                (container_key.clone(), format!("${{{source_key}}}"))
            })
            .collect();
        if let Some(preset) = &service.preset {
            let catalog_service = catalog.find(preset).ok_or_else(|| {
                anyhow::anyhow!("service `{name}` uses unknown preset `{preset}`")
            })?;
            add_catalog_environment(&mut environment, catalog_service);
        }
        services.insert(
            name.clone(),
            ComposeService {
                image,
                container_name: manifest.container_name(name),
                ports: service.ports.iter().map(ToString::to_string).collect(),
                volumes: service.mounts.iter().map(ToString::to_string).collect(),
                environment,
                depends_on: service
                    .depends_on
                    .iter()
                    .filter(|dependency| selected.contains(*dependency))
                    .cloned()
                    .collect(),
                command: service.command.clone(),
                network_mode: service.network.clone(),
                restart: service.restart.map(|restart| restart.to_string()),
                healthcheck: compose_healthcheck(service.health.as_ref()),
            },
        );
    }
    let mut output =
        String::from("# Generated by Asur. Secret values are resolved by Compose at runtime.\n");
    output.push_str(&serde_yaml::to_string(&ComposeDocument { services })?);
    Ok(output)
}

fn add_catalog_environment(environment: &mut BTreeMap<String, String>, service: &Service) {
    if let (Some(key), Some(value)) = (&service.user_env, &service.default_user) {
        environment
            .entry(key.clone())
            .or_insert_with(|| value.clone());
    }
    if let (Some(key), Some(value)) = (&service.database_env, &service.default_database) {
        environment
            .entry(key.clone())
            .or_insert_with(|| value.clone());
    }
    if let Some(key) = &service.password_env {
        environment
            .entry(key.clone())
            .or_insert_with(|| format!("${{{key}:?set {key}}}"));
    }
    for entry in &service.extra_env {
        if let Some((key, value)) = entry.split_once('=') {
            environment
                .entry(key.to_string())
                .or_insert_with(|| value.to_string());
        }
    }
}

fn service_image(service: &ServiceConfig, catalog: &Catalog, name: &str) -> Result<String> {
    if let Some(image) = &service.image {
        return Ok(image.clone());
    }
    let preset = service.preset.as_deref().expect("validated preset service");
    let catalog_service = catalog
        .find(preset)
        .ok_or_else(|| anyhow::anyhow!("service `{name}` uses unknown preset `{preset}`"))?;
    Ok(format!(
        "{}:{}",
        catalog_service.image, catalog_service.default_tag
    ))
}

fn compose_healthcheck(health: Option<&HealthCheck>) -> Option<ComposeHealthcheck> {
    match health? {
        HealthCheck::Tcp {
            port,
            timeout_seconds,
        } => Some(ComposeHealthcheck {
            test: vec![
                "CMD-SHELL".into(),
                format!("(echo > /dev/tcp/127.0.0.1/{port}) >/dev/null 2>&1"),
            ],
            interval: "2s".into(),
            timeout: "1s".into(),
            retries: (*timeout_seconds).div_ceil(2).max(1),
            start_period: "0s".into(),
        }),
        HealthCheck::Command {
            command,
            interval_seconds,
            timeout_seconds,
            retries,
            start_period_seconds,
        } => Some(ComposeHealthcheck {
            test: std::iter::once("CMD".to_string())
                .chain(command.iter().cloned())
                .collect(),
            interval: format!("{interval_seconds}s"),
            timeout: format!("{timeout_seconds}s"),
            retries: *retries,
            start_period: format!("{start_period_seconds}s"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ServiceConfig;

    fn manifest() -> ProjectManifest {
        let mut manifest = ProjectManifest::new("shop");
        manifest.services.insert(
            "db".into(),
            ServiceConfig {
                preset: Some("postgres".into()),
                environment: BTreeMap::from([("POSTGRES_PASSWORD".into(), "DB_PASSWORD".into())]),
                ..Default::default()
            },
        );
        manifest.services.insert(
            "api".into(),
            ServiceConfig {
                image: Some("ghcr.io/acme/api:1.2.3".into()),
                depends_on: vec!["db".into()],
                ..Default::default()
            },
        );
        manifest
    }

    #[test]
    fn compose_export_keeps_secret_references_only() {
        let yaml = compose_yaml(&manifest(), None).unwrap();
        assert!(yaml.contains("${DB_PASSWORD}"));
        assert!(!yaml.contains("secret-value"));
        assert!(yaml.contains("ghcr.io/acme/api:1.2.3"));
    }

    #[test]
    fn environment_overrides_replace_generated_values() {
        let mut values = vec!["PASSWORD=generated".into(), "MODE=dev".into()];
        merge_environment(&mut values, vec!["PASSWORD=provided".into()]);
        assert_eq!(values, ["MODE=dev", "PASSWORD=provided"]);
    }
}
