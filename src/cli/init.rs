//! Discover a project environment and write a reviewable `.asur.toml`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::catalog::Catalog;
use crate::docker::{ContainerRuntime, DockerRuntime, PortBinding};
use crate::env::parser::{EnvFile, EnvLine};
use crate::project::{
    MANIFEST_FILE, ProfileConfig, ProjectManifest, ServiceConfig, import_compose, sanitize_name,
};
use crate::ui;

#[derive(Args, Debug, Clone, Default)]
pub struct InitArgs {
    /// Compose file to import (auto-detected when omitted)
    #[arg(long, value_name = "FILE")]
    pub compose: Option<PathBuf>,
    /// Do not inspect running Asur containers
    #[arg(long)]
    pub no_containers: bool,
    /// Replace an existing .asur.toml
    #[arg(long)]
    pub force: bool,
    /// Accept the generated manifest without an interactive confirmation
    #[arg(short = 'y', long)]
    pub yes: bool,
}

pub async fn run(args: InitArgs) -> Result<()> {
    let root = std::env::current_dir().context("could not determine the project directory")?;
    let destination = root.join(MANIFEST_FILE);
    if destination.exists() && !args.force {
        bail!(
            "`{}` already exists; review it or rerun with `--force` to replace it",
            destination.display()
        );
    }

    let catalog = Catalog::load()?;
    let project_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .map(sanitize_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "project".to_string());
    let mut manifest = ProjectManifest::new(project_name);
    manifest.project.env_files = detect_env_files(&root);
    let env_values = read_env_values(&root, &manifest.project.env_files)?;
    let mut warnings = Vec::new();

    if let Some(compose_path) = args.compose.or_else(|| find_compose_file(&root)) {
        let compose_path = if compose_path.is_absolute() {
            compose_path
        } else {
            root.join(compose_path)
        };
        let imported = import_compose(&compose_path, &catalog)?;
        manifest.services.extend(imported.services);
        manifest.profiles.extend(imported.profiles);
        warnings.extend(imported.warnings);
        ui::info(&format!("Imported `{}`.", compose_path.display()));
    }

    detect_catalog_services(&mut manifest, &catalog, &env_values);

    if !args.no_containers {
        match DockerRuntime::connect() {
            Ok(runtime) => {
                if let Err(error) = import_local_containers(
                    &runtime,
                    &catalog,
                    &env_values,
                    &mut manifest,
                    &mut warnings,
                )
                .await
                {
                    warnings.push(format!(
                        "Docker inspection was skipped: {error:#}. The Compose and env-file results are still usable"
                    ));
                }
            }
            Err(error) => warnings.push(format!(
                "Docker inspection was skipped: {error}. The Compose and env-file results are still usable"
            )),
        }
    }

    if manifest.services.is_empty() {
        bail!(
            "no services were detected. Add a Compose file, start an Asur container, or define a service in `{MANIFEST_FILE}`"
        );
    }
    manifest
        .profiles
        .entry("full".into())
        .or_insert_with(|| ProfileConfig {
            description: "All project services".into(),
            services: manifest.services.keys().cloned().collect(),
        });
    manifest.validate()?;

    println!("Project environment preview:");
    for (name, service) in &manifest.services {
        let source = service
            .preset
            .as_deref()
            .map(|preset| format!("preset {preset}"))
            .or_else(|| {
                service
                    .image
                    .as_deref()
                    .map(|image| format!("image {image}"))
            })
            .unwrap_or_default();
        let ports = if service.ports.is_empty() {
            "no published ports".to_string()
        } else {
            service
                .ports
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!("  {name:<18} {source:<36} {ports}");
    }
    for warning in &warnings {
        ui::warn(warning);
    }

    let interactive = std::io::stdin().is_terminal();
    if !args.yes && interactive {
        let confirmed = inquire::Confirm::new("Write .asur.toml with this configuration?")
            .with_default(true)
            .with_help_message("Esc: cancel and exit")
            .prompt()
            .map_err(super::prompt_error)?;
        if !confirmed {
            return Err(super::UserCancelled.into());
        }
    } else if !args.yes {
        bail!("non-interactive initialization requires `--yes`");
    }

    std::fs::write(&destination, manifest.to_toml()?)
        .with_context(|| format!("could not write `{}`", destination.display()))?;
    write_env_example(&root, &manifest)?;
    ui::success(&format!("Created `{}`.", destination.display()));
    ui::hint("Run `asur dev up`, then `asur dev status`.");
    Ok(())
}

fn detect_env_files(root: &Path) -> Vec<PathBuf> {
    let candidates = [".env", ".env.local", ".env.development", ".env.dev"];
    let mut found = Vec::new();
    for candidate in candidates {
        let path = root.join(candidate);
        if path.is_file() {
            found.push(PathBuf::from(candidate));
        }
    }
    if found.is_empty() {
        found.push(PathBuf::from(".env"));
    }
    found
}

fn read_env_values(root: &Path, files: &[PathBuf]) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    for file in files {
        let path = root.join(file);
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("could not read `{}`", path.display()));
            }
        };
        for line in EnvFile::parse(&content).lines {
            if let EnvLine::Entry { key, value, .. } = line {
                values.insert(key, value);
            }
        }
    }
    Ok(values)
}

fn find_compose_file(root: &Path) -> Option<PathBuf> {
    [
        "compose.yaml",
        "compose.yml",
        "docker-compose.yaml",
        "docker-compose.yml",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| root.join(path).is_file())
}

fn detect_catalog_services(
    manifest: &mut ProjectManifest,
    catalog: &Catalog,
    env: &BTreeMap<String, String>,
) {
    for service in catalog.services() {
        let service_envs = [
            service.user_env.as_deref(),
            service.password_env.as_deref(),
            service.database_env.as_deref(),
        ];
        let detected = service_envs
            .into_iter()
            .flatten()
            .any(|key| env.contains_key(key));
        if !detected || manifest.services.contains_key(&service.name) {
            continue;
        }
        let environment = service_envs
            .into_iter()
            .flatten()
            .filter(|key| env.contains_key(*key))
            .map(|key| (key.to_string(), key.to_string()))
            .collect();
        manifest.services.insert(
            service.name.clone(),
            ServiceConfig {
                preset: Some(service.name.clone()),
                ports: vec![PortBinding::tcp(service.default_port, service.default_port)],
                environment,
                ..Default::default()
            },
        );
    }
}

async fn import_local_containers<R: ContainerRuntime>(
    runtime: &R,
    catalog: &Catalog,
    env_values: &BTreeMap<String, String>,
    manifest: &mut ProjectManifest,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let containers = runtime.list_containers(true).await?;
    for container in containers
        .into_iter()
        .filter(|container| container.name.starts_with("asur-"))
    {
        let raw_name = container.name.trim_start_matches("asur-");
        let name = unique_service_name(manifest, &sanitize_name(raw_name));
        if name.is_empty() {
            continue;
        }
        let preset = catalog.services().iter().find(|service| {
            container.image == format!("{}:{}", service.image, service.default_tag)
        });
        let image = if preset.is_some() {
            None
        } else {
            match runtime.inspect_image(&container.image).await {
                Ok(metadata) => metadata
                    .digests
                    .first()
                    .cloned()
                    .or_else(|| Some(container.image.clone())),
                Err(_) => Some(container.image.clone()),
            }
        };
        let details = runtime.get_container_details(&container.name).await?;
        let environment = details
            .env
            .iter()
            .filter_map(|entry| entry.split_once('='))
            .filter(|(key, value)| env_values.get(*key).is_some_and(|current| current == value))
            .map(|(key, _)| (key.to_string(), key.to_string()))
            .collect();
        let ports = container
            .ports
            .iter()
            .filter_map(|port| parse_runtime_port(port))
            .collect();
        manifest.services.insert(
            name.clone(),
            ServiceConfig {
                preset: preset.map(|service| service.name.clone()),
                image,
                container_name: Some(container.name.clone()),
                ports,
                environment,
                ..Default::default()
            },
        );
        warnings.push(format!(
            "imported local container `{}` as `{name}`; verify its mounts and dependencies",
            container.name
        ));
    }
    Ok(())
}

fn unique_service_name(manifest: &ProjectManifest, proposed: &str) -> String {
    if !manifest.services.contains_key(proposed) {
        return proposed.to_string();
    }
    for suffix in 2..1000 {
        let candidate = format!("{proposed}-{suffix}");
        if !manifest.services.contains_key(&candidate) {
            return candidate;
        }
    }
    String::new()
}

fn parse_runtime_port(value: &str) -> Option<PortBinding> {
    let (host, container) = value.split_once("->")?;
    let host = host.rsplit(':').next()?;
    let (container, protocol) = container.split_once('/').unwrap_or((container, "tcp"));
    format!("{host}:{container}/{protocol}").parse().ok()
}

fn write_env_example(root: &Path, manifest: &ProjectManifest) -> Result<()> {
    let destination = root.join(".env.example");
    if destination.exists() {
        return Ok(());
    }
    let keys: BTreeSet<_> = manifest
        .services
        .values()
        .flat_map(|service| service.environment.values().cloned())
        .collect();
    if keys.is_empty() {
        return Ok(());
    }
    let content = keys
        .into_iter()
        .map(|key| format!("{key}=\n"))
        .collect::<String>();
    std::fs::write(&destination, content)
        .with_context(|| format!("could not write `{}`", destination.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_ports_become_manifest_bindings() {
        assert_eq!(
            parse_runtime_port("0.0.0.0:5432->5432/tcp"),
            Some(PortBinding::tcp(5432, 5432))
        );
        assert!(parse_runtime_port("5432/tcp").is_none());
    }

    #[test]
    fn env_detection_never_copies_values() {
        let mut manifest = ProjectManifest::new("demo");
        let catalog = Catalog::load().unwrap();
        let env = BTreeMap::from([("POSTGRES_PASSWORD".into(), "top-secret".into())]);
        detect_catalog_services(&mut manifest, &catalog, &env);
        let postgres = &manifest.services["postgres"];
        assert_eq!(
            postgres.environment.get("POSTGRES_PASSWORD"),
            Some(&"POSTGRES_PASSWORD".to_string())
        );
        assert!(!manifest.to_toml().unwrap().contains("top-secret"));
    }
}
