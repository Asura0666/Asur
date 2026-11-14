//! Container spin-up wizard (`asur up`).

mod compose;
mod config;

pub use compose::generate_compose_yaml;
pub use config::{CustomOptions, LaunchSummary, build_container_config};

use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use anyhow::{Context, Result, bail};
use clap::Args;
use owo_colors::{OwoColorize, Stream, Style};

use crate::catalog::{Catalog, Service};
use crate::docker::{
    ContainerRuntime, DockerRuntime, ImageMetadata, ImageReference, ImageTag, Mount,
    OperationEvent, OperationRecord, OperationStatus, PortBinding, PortProtocol, PullPolicy,
    RestartPolicy,
};
use crate::env::parser::{EnvFile, EnvLine};
use crate::ui;
use crate::util::clipboard::copy_to_clipboard;
use crate::util::ports::{
    extract_docker_ports, find_available_port, is_port_available, is_port_free,
};

#[derive(Args, Debug, Clone, Default)]
pub struct UpArgs {
    /// Services to start, e.g. `postgres redis` (omit to pick from an interactive list)
    pub services: Vec<String>,
    /// Force one target to run as a raw OCI image instead of a catalog preset
    #[arg(long, value_name = "REFERENCE")]
    pub image: Option<String>,
    /// Skip questions: use defaults and auto-generated credentials
    #[arg(short, long)]
    pub quick: bool,
    /// Show advanced options (custom port, password, volume, etc.)
    #[arg(long)]
    pub advanced: bool,
    /// Generate a docker-compose.yml from the chosen services
    #[arg(long)]
    pub compose: bool,
    /// Container name (generic-image mode)
    #[arg(long)]
    pub name: Option<String>,
    /// Publish HOST_PORT:CONTAINER_PORT[/tcp|udp], optionally prefixed by HOST_IP
    #[arg(short = 'p', long = "publish", value_name = "MAPPING")]
    pub publish: Vec<PortBinding>,
    /// Set KEY=VALUE, or copy KEY from the current process environment
    #[arg(short = 'e', long = "env", value_name = "KEY[=VALUE]")]
    pub environment: Vec<String>,
    /// Import environment variables from a file (later files and --env win)
    #[arg(long = "env-file", value_name = "PATH")]
    pub env_files: Vec<PathBuf>,
    /// Mount SOURCE:TARGET[:ro] or type=bind|volume,source=...,target=...
    #[arg(long, value_name = "MOUNT")]
    pub mount: Vec<Mount>,
    /// Connect the container to a Docker network or network mode
    #[arg(long)]
    pub network: Option<String>,
    /// Restart policy: no, on-failure[:N], always, or unless-stopped
    #[arg(long)]
    pub restart: Option<RestartPolicy>,
    /// Override one image command argument; repeat for each argument
    #[arg(long, action = clap::ArgAction::Append, allow_hyphen_values = true, value_name = "ARG")]
    pub command: Vec<String>,
    /// Image pull behavior
    #[arg(long, default_value = "missing")]
    pub pull: PullPolicy,
    /// Save the resolved immutable digest to .asur.toml after a successful launch
    #[arg(long)]
    pub pin: bool,
}

pub async fn run(args: UpArgs) -> Result<()> {
    let catalog = Catalog::load()?;
    let runtime = Arc::new(DockerRuntime::connect()?);
    let is_interactive = std::io::stdin().is_terminal();
    let mut operations = OperationManager::default();
    if args.image.is_some() && !args.services.is_empty() {
        bail!("use either `asur up <target>` or `asur up --image <reference>`, not both");
    }
    let forced_image = args.image.is_some();
    let mut initial_targets = args
        .image
        .clone()
        .map(|image| vec![image])
        .unwrap_or_else(|| args.services.clone());
    if initial_targets.len() > 1 && has_generic_runtime_options(&args) {
        bail!(
            "generic runtime options apply to one image at a time; launch each custom image with a separate `asur up` command"
        );
    }

    'configure: loop {
        // Query existing containers from Docker to discover already allocated ports & names
        operations.drain_updates();
        let existing_containers = runtime.list_containers(true).await?;
        let mut docker_ports = extract_docker_ports(&existing_containers);
        for port in &operations.reserved_ports {
            docker_ports
                .entry(*port)
                .or_insert_with(|| "an active Asur operation".to_string());
        }
        let mut existing_names: HashSet<String> =
            existing_containers.iter().map(|c| c.name.clone()).collect();
        existing_names.extend(operations.reserved_names.iter().cloned());

        // Resolve the service to configure in this pass
        let target = if !initial_targets.is_empty() {
            let name = initial_targets.remove(0);
            resolve_target(
                &name,
                &catalog,
                runtime.as_ref(),
                is_interactive,
                forced_image,
            )
            .await?
        } else if is_interactive {
            ResolvedTarget::Preset(select_service_interactive(&catalog)?)
        } else {
            bail!("no target specified. Run `asur up <preset>` or `asur up --image <reference>`");
        };

        let (container_config, summary) = match target {
            ResolvedTarget::Preset(service) => {
                if has_generic_runtime_options(&args) {
                    bail!(
                        "`--name`, `--publish`, `--env`, `--env-file`, `--mount`, `--network`, `--restart`, `--command`, `--pull`, and `--pin` are generic-image options; use `--image {}` to force raw-image mode",
                        service.name
                    );
                }
                let options = if args.quick {
                    CustomOptions::default()
                } else if is_interactive {
                    prompt_service_options(service, args.advanced, &docker_ports, &existing_names)?
                } else {
                    CustomOptions::default()
                };
                build_container_config(service, &options)
            }
            ResolvedTarget::Generic(image) => {
                if image.uses_latest() {
                    ui::warn(&format!(
                        "`{image}` uses the mutable `latest` tag. Prefer a version tag or `--pin` for reproducibility."
                    ));
                }
                let metadata = prepare_generic_image(runtime.as_ref(), &image, args.pull).await?;
                let options = build_generic_options(
                    &args,
                    image,
                    &metadata,
                    is_interactive,
                    &docker_ports,
                    &existing_names,
                )?;
                config::build_generic_config(options)
            }
        };

        let operation_id = operations.start(runtime.clone(), container_config, summary);
        ui::info(&format!(
            "Queued operation #{operation_id}. Continue configuring services while it runs."
        ));

        // If command-line specified multiple services, continue through them
        if !initial_targets.is_empty() {
            continue;
        }

        // Interactive follow-up options: done, compose, or spin another
        if is_interactive {
            let actions = vec![
                "Done / Exit (wait for operations)",
                "Spin up another service",
                "Show operation status",
                "Generate docker-compose.yml (wait for operations)",
            ];

            loop {
                let prompt =
                    inquire::Select::new("What would you like to do next?", actions.clone())
                        .with_help_message("Esc: stop configuring and wait for current operations")
                        .prompt();
                let action = match prompt {
                    Ok(action) => action,
                    Err(inquire::error::InquireError::OperationCanceled)
                    | Err(inquire::error::InquireError::OperationInterrupted) => break 'configure,
                    Err(error) => return Err(error.into()),
                };

                match action {
                    "Spin up another service" => continue 'configure,
                    "Show operation status" => operations.render_status(),
                    "Generate docker-compose.yml (wait for operations)" => {
                        operations.wait_all().await?;
                        write_compose_file(&operations.completed_summaries)?;
                        break 'configure;
                    }
                    _ => break 'configure,
                }
            }
        } else {
            break 'configure;
        }
    }

    operations.wait_all().await?;
    if args.compose && !operations.completed_summaries.is_empty() {
        write_compose_file(&operations.completed_summaries)?;
    }

    Ok(())
}

enum ResolvedTarget<'a> {
    Preset(&'a Service),
    Generic(ImageReference),
}

fn has_generic_runtime_options(args: &UpArgs) -> bool {
    args.name.is_some()
        || !args.publish.is_empty()
        || !args.environment.is_empty()
        || !args.env_files.is_empty()
        || !args.mount.is_empty()
        || args.network.is_some()
        || args.restart.is_some()
        || !args.command.is_empty()
        || args.pull != PullPolicy::Missing
        || args.pin
}

async fn resolve_target<'a, R: ContainerRuntime>(
    name: &str,
    catalog: &'a Catalog,
    runtime: &R,
    interactive: bool,
    force_image: bool,
) -> Result<ResolvedTarget<'a>> {
    if force_image || ImageReference::is_qualified_target(name) {
        return Ok(ResolvedTarget::Generic(name.parse()?));
    }
    if let Some(service) = catalog.find(name) {
        return Ok(ResolvedTarget::Preset(service));
    }
    if let Some(suggested) = catalog.suggest(name) {
        if interactive {
            let confirmed = inquire::Confirm::new(&format!(
                "Unknown service `{name}`. Did you mean catalog preset `{}`?",
                suggested.name
            ))
            .with_default(true)
            .prompt()
            .map_err(super::prompt_error)?;
            if confirmed {
                return Ok(ResolvedTarget::Preset(suggested));
            }
        } else {
            bail!(
                "unknown catalog service `{name}`. Did you mean `{}`? Use `--image {name}` to launch it as a raw image",
                suggested.name
            );
        }
    }
    if !interactive {
        bail!(
            "unknown catalog service `{name}`. Use `asur up --image {name}` to explicitly launch a raw image"
        );
    }

    let action = inquire::Select::new(
        &format!("`{name}` is not a catalog preset. What should Asur do?"),
        vec!["Search Docker Hub", "Launch as a raw image"],
    )
    .with_help_message("Esc: cancel and exit")
    .prompt()
    .map_err(super::prompt_error)?;
    match action {
        "Search Docker Hub" => {
            let results = runtime.search_images(name, 20).await?;
            if results.is_empty() {
                bail!("Docker Hub returned no results for `{name}`");
            }
            let names: Vec<String> = results.into_iter().map(|result| result.name).collect();
            let selected = inquire::Select::new("Choose an image:", names)
                .with_help_message("Esc: cancel and exit")
                .prompt()
                .map_err(super::prompt_error)?;
            let tags = runtime.list_image_tags(&selected, 200).await?;
            if tags.is_empty() {
                bail!("Docker Hub returned no pullable tags for `{selected}`");
            }
            let tag_names = preferred_stable_tags(&tags);
            let prompt = if tag_names.iter().any(|tag| numeric_version(tag).is_some()) {
                "Choose a stable version tag:"
            } else {
                "Choose a tag:"
            };
            let tag = inquire::Select::new(prompt, tag_names)
                .with_help_message(
                    "Docker Hub does not provide LTS status. Pre-release and CPU-specific tags are hidden. Esc: cancel",
                )
                .prompt()
                .map_err(super::prompt_error)?;
            Ok(ResolvedTarget::Generic(
                format!("{selected}:{tag}").parse()?,
            ))
        }
        "Launch as a raw image" => Ok(ResolvedTarget::Generic(name.parse()?)),
        _ => Err(super::UserCancelled.into()),
    }
}

fn preferred_stable_tags(tags: &[ImageTag]) -> Vec<String> {
    let mut versioned: Vec<(Vec<u64>, String)> = tags
        .iter()
        .filter(|tag| !is_prerelease_tag(&tag.name) && !is_platform_specific_tag(&tag.name))
        .filter_map(|tag| numeric_version(&tag.name).map(|version| (version, tag.name.clone())))
        .collect();
    versioned.sort_by(|(left, _), (right, _)| right.cmp(left));
    versioned.dedup_by(|(_, left), (_, right)| left == right);
    if !versioned.is_empty() {
        return versioned
            .into_iter()
            .take(15)
            .map(|(_, name)| name)
            .collect();
    }

    let mut stable: Vec<String> = tags
        .iter()
        .filter(|tag| !is_prerelease_tag(&tag.name) && !is_platform_specific_tag(&tag.name))
        .map(|tag| tag.name.clone())
        .collect();
    stable.dedup();
    if stable.is_empty() {
        stable = tags.iter().map(|tag| tag.name.clone()).collect();
        stable.dedup();
    }
    stable.truncate(15);
    stable
}

fn numeric_version(tag: &str) -> Option<Vec<u64>> {
    let value = tag.strip_prefix('v').unwrap_or(tag);
    if value.is_empty()
        || !value
            .chars()
            .all(|character| character.is_ascii_digit() || character == '.')
    {
        return None;
    }
    value
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect()
}

fn is_prerelease_tag(tag: &str) -> bool {
    let tag = tag.to_ascii_lowercase();
    [
        "rc", "alpha", "beta", "dev", "nightly", "snapshot", "canary", "edge",
    ]
    .iter()
    .any(|marker| tag.contains(marker))
}

fn is_platform_specific_tag(tag: &str) -> bool {
    let tag = tag.to_ascii_lowercase();
    [
        "amd64", "arm64", "armv7", "armv8", "x86_64", "ppc64", "s390x",
    ]
    .iter()
    .any(|platform| tag.contains(platform))
}

async fn prepare_generic_image(
    runtime: &DockerRuntime,
    image: &ImageReference,
    pull_policy: PullPolicy,
) -> Result<ImageMetadata> {
    let spinner = ui::Spinner::new(format!("Preparing image {image}"));
    let mut progress: Option<ui::Progress> = None;
    let mut pull_layers: HashMap<String, (u64, u64)> = HashMap::new();
    let mut displayed_percent = 0_u64;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let preparation = runtime.prepare_image(image, pull_policy, tx);
    tokio::pin!(preparation);
    loop {
        tokio::select! {
            result = &mut preparation => {
                match result {
                    Ok(metadata) => {
                        if let Some(progress) = progress {
                            progress.finish(format!("Image {image} is ready"));
                        } else {
                            spinner.finish(format!("Image {image} is ready"));
                        }
                        return Ok(metadata);
                    }
                    Err(error) => {
                        spinner.clear();
                        if let Some(progress) = progress {
                            progress.clear();
                        }
                        return Err(error);
                    }
                }
            }
            Some(event) = rx.recv() => {
                if let (Some(current), Some(total)) = (event.current_bytes, event.total_bytes)
                    && current >= 0
                    && total > 0
                {
                    pull_layers.insert(
                        event.layer_id.unwrap_or_else(|| "pull".to_string()),
                        (current as u64, total as u64),
                    );
                    let (downloaded, total) = pull_layers.values().fold(
                        (0_u64, 0_u64),
                        |(downloaded, total), (layer_downloaded, layer_total)| {
                            (
                                downloaded.saturating_add(*layer_downloaded),
                                total.saturating_add(*layer_total),
                            )
                        },
                    );
                    if progress.is_none() {
                        spinner.clear();
                        progress = Some(ui::Progress::new(
                            format!("Pulling {image}"),
                            total,
                        ));
                    }
                    let percent = downloaded.saturating_mul(100) / total.max(1);
                    displayed_percent = displayed_percent.max(percent.min(99));
                    if let Some(progress) = &progress {
                        progress.set_percent(displayed_percent);
                    }
                } else if progress.is_none()
                    && matches!(event.status, OperationStatus::CheckingImage | OperationStatus::Pulling)
                {
                    spinner.set_message(event.detail);
                }
            }
        }
    }
}

fn build_generic_options(
    args: &UpArgs,
    image: ImageReference,
    metadata: &ImageMetadata,
    interactive: bool,
    docker_ports: &HashMap<u16, String>,
    existing_names: &HashSet<String>,
) -> Result<config::GenericOptions> {
    show_image_suggestions(metadata);
    let default_name = image.suggested_container_name();
    let container_name = if let Some(name) = &args.name {
        validate_container_name(name, existing_names)?;
        name.clone()
    } else if interactive {
        prompt_container_name(&default_name, existing_names)?
    } else {
        validate_container_name(&default_name, existing_names)?;
        default_name
    };

    let ports = if args.publish.is_empty() && interactive {
        prompt_declared_ports(metadata, docker_ports)?
    } else {
        validate_port_bindings(&args.publish, docker_ports)?;
        args.publish.clone()
    };
    let mounts = if args.mount.is_empty() && interactive {
        prompt_declared_mounts(metadata, &container_name)?
    } else {
        args.mount.clone()
    };

    Ok(config::GenericOptions {
        image,
        container_name,
        env: load_environment(&args.env_files, &args.environment)?,
        ports,
        mounts,
        network: args.network.clone(),
        restart: args.restart.unwrap_or(RestartPolicy::No),
        command: (!args.command.is_empty()).then(|| args.command.clone()),
        // The image was prepared immediately before this configuration was built.
        pull_policy: PullPolicy::Never,
        resolved_digest: metadata.digests.first().cloned(),
        pin: args.pin,
    })
}

fn show_image_suggestions(metadata: &ImageMetadata) {
    if !metadata.exposed_ports.is_empty() {
        ui::info(&format!(
            "This image listens on {} inside its container.",
            metadata.exposed_ports.join(", ")
        ));
        ui::hint(
            "Publish a port to reach the service from your computer or browser. Without it, only containers on the same Docker network can connect.",
        );
    }
    if !metadata.declared_volumes.is_empty() {
        ui::info(&format!(
            "Image declares volumes: {}. Nothing is mounted unless you select or pass `--mount`.",
            metadata.declared_volumes.join(", ")
        ));
    }
}

fn validate_container_name(name: &str, existing_names: &HashSet<String>) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 255
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if !valid {
        bail!(
            "invalid container name `{name}`; use letters, digits, `_`, `.`, or `-`, beginning with a letter or digit"
        );
    }
    if existing_names.contains(name) {
        bail!("a container named `{name}` already exists; choose another name with `--name`");
    }
    Ok(())
}

fn prompt_container_name(default_name: &str, existing_names: &HashSet<String>) -> Result<String> {
    let names = existing_names.clone();
    inquire::Text::new("Container name:")
        .with_default(default_name)
        .with_validator(
            move |input: &str| match validate_container_name(input, &names) {
                Ok(()) => Ok(inquire::validator::Validation::Valid),
                Err(error) => Ok(inquire::validator::Validation::Invalid(
                    error.to_string().into(),
                )),
            },
        )
        .prompt()
        .map_err(super::prompt_error)
}

fn validate_port_bindings(
    bindings: &[PortBinding],
    docker_ports: &HashMap<u16, String>,
) -> Result<()> {
    let mut requested = HashSet::new();
    for binding in bindings {
        if !requested.insert((binding.host_ip.clone(), binding.host_port, binding.protocol)) {
            bail!("duplicate published port `{binding}`");
        }
        if let Some(owner) = docker_ports.get(&binding.host_port) {
            bail!(
                "host port {} is already used by container `{owner}`",
                binding.host_port
            );
        }
        if binding.protocol == PortProtocol::Tcp && !is_port_free(binding.host_port) {
            bail!("host TCP port {} is already in use", binding.host_port);
        }
    }
    Ok(())
}

fn prompt_declared_ports(
    metadata: &ImageMetadata,
    docker_ports: &HashMap<u16, String>,
) -> Result<Vec<PortBinding>> {
    if metadata.exposed_ports.is_empty() {
        return Ok(Vec::new());
    }
    let selection_formatter =
        |items: &[inquire::list_option::ListOption<&String>]| match items.len() {
            1 => "1 port selected".to_string(),
            count => format!("{count} ports selected"),
        };
    let selected = inquire::MultiSelect::new(
        "Select ports to make accessible from this computer:",
        metadata.exposed_ports.clone(),
    )
    .with_all_selected_by_default()
    .with_help_message(
        "Ports are selected by default. Space: unselect  Enter: continue  Esc: cancel",
    )
    .with_formatter(&selection_formatter)
    .prompt()
    .map_err(super::prompt_error)?;
    let mut bindings = Vec::new();
    for value in selected {
        let (port, protocol) = value.split_once('/').unwrap_or((&value, "tcp"));
        let container_port = port
            .parse::<u16>()
            .with_context(|| format!("image declared invalid port `{value}`"))?;
        let protocol = protocol.parse::<PortProtocol>()?;
        let suggested =
            if protocol == PortProtocol::Tcp && !is_port_available(container_port, docker_ports) {
                find_available_port(container_port, docker_ports)
            } else {
                container_port
            };
        let host_port =
            inquire::CustomType::<u16>::new(&format!("Host port for {container_port}/{protocol}:"))
                .with_default(suggested)
                .prompt()
                .map_err(super::prompt_error)?;
        bindings.push(PortBinding {
            host_ip: None,
            host_port,
            container_port,
            protocol,
        });
    }
    validate_port_bindings(&bindings, docker_ports)?;
    Ok(bindings)
}

fn prompt_declared_mounts(metadata: &ImageMetadata, container_name: &str) -> Result<Vec<Mount>> {
    if metadata.declared_volumes.is_empty() {
        return Ok(Vec::new());
    }
    let selection_formatter =
        |items: &[inquire::list_option::ListOption<&String>]| match items.len() {
            1 => "1 volume selected".to_string(),
            count => format!("{count} volumes selected"),
        };
    let selected = inquire::MultiSelect::new(
        "Select declared volumes to mount (none are selected by default):",
        metadata.declared_volumes.clone(),
    )
    .with_default(&[])
    .with_help_message("Space: select  Enter: continue  Esc: cancel")
    .with_formatter(&selection_formatter)
    .prompt()
    .map_err(super::prompt_error)?;
    Ok(selected
        .into_iter()
        .enumerate()
        .map(|(index, target)| {
            Mount::volume(format!("{container_name}-data-{}", index + 1), target)
        })
        .collect())
}

fn load_environment(files: &[PathBuf], explicit: &[String]) -> Result<Vec<String>> {
    let mut values = std::collections::BTreeMap::new();
    for path in files {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("could not read env file `{}`", path.display()))?;
        let parsed = EnvFile::parse(&content);
        for line in parsed.lines {
            if let EnvLine::Entry { key, value, .. } = line {
                validate_env_key(&key)?;
                values.insert(key, value);
            }
        }
    }
    for item in explicit {
        let (key, value) = match item.split_once('=') {
            Some((key, value)) => (key.to_string(), value.to_string()),
            None => (
                item.clone(),
                std::env::var(item).with_context(|| {
                    format!("environment variable `{item}` is not set in the current process")
                })?,
            ),
        };
        validate_env_key(&key)?;
        values.insert(key, value);
    }
    Ok(values
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect())
}

fn validate_env_key(key: &str) -> Result<()> {
    let valid = key
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid {
        bail!("invalid environment variable name `{key}`");
    }
    Ok(())
}

/// Owns every background launch for the lifetime of `asur up`.  It is
/// intentionally process-local: closing the CLI waits for operations rather
/// than falsely implying that a detached task will survive process exit.
struct OperationManager {
    next_id: u64,
    records: HashMap<u64, OperationRecord>,
    reserved_names: HashSet<String>,
    reserved_ports: HashSet<u16>,
    permits: Arc<Semaphore>,
    joins: JoinSet<(
        u64,
        LaunchSummary,
        Result<crate::docker::ContainerInfo>,
        Option<ImageMetadata>,
    )>,
    events_tx: Option<tokio::sync::mpsc::UnboundedSender<(u64, OperationEvent)>>,
    events_rx: Option<tokio::sync::mpsc::UnboundedReceiver<(u64, OperationEvent)>>,
    byte_progress: HashMap<u64, ui::Progress>,
    pull_layers: HashMap<u64, HashMap<String, (u64, u64)>>,
    displayed_percent: HashMap<u64, u64>,
    completed_summaries: Vec<LaunchSummary>,
    failures: Vec<String>,
}

impl Default for OperationManager {
    fn default() -> Self {
        Self {
            next_id: 0,
            records: HashMap::new(),
            reserved_names: HashSet::new(),
            reserved_ports: HashSet::new(),
            // Docker pulls and image decompression are resource-heavy. Four
            // active operations keep the UI responsive without unbounded work.
            permits: Arc::new(Semaphore::new(4)),
            joins: JoinSet::new(),
            events_tx: None,
            events_rx: None,
            byte_progress: HashMap::new(),
            pull_layers: HashMap::new(),
            displayed_percent: HashMap::new(),
            completed_summaries: Vec::new(),
            failures: Vec::new(),
        }
    }
}

impl OperationManager {
    fn start(
        &mut self,
        runtime: Arc<DockerRuntime>,
        config: crate::docker::CreateContainerConfig,
        summary: LaunchSummary,
    ) -> u64 {
        if self.events_tx.is_none() {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            self.events_tx = Some(tx);
            self.events_rx = Some(rx);
        }
        self.next_id += 1;
        let id = self.next_id;
        self.reserved_names.insert(config.name.clone());
        self.reserved_ports
            .extend(config.ports.iter().map(|binding| binding.host_port));
        self.records.insert(
            id,
            OperationRecord {
                id,
                service: summary.service_name.clone(),
                resource: config.name.clone(),
                image: config.image.clone(),
                created_at: SystemTime::now(),
                status: OperationStatus::Queued,
                detail: format!("{} ({})", config.name, config.image),
                error: None,
            },
        );
        let tx = self
            .events_tx
            .as_ref()
            .expect("operation event channel initialized")
            .clone();
        let permits = self.permits.clone();
        self.joins.spawn(async move {
            let (operation_tx, mut operation_rx) = tokio::sync::mpsc::unbounded_channel();
            let forwarder = tokio::spawn(async move {
                while let Some(event) = operation_rx.recv().await {
                    let _ = tx.send((id, event));
                }
            });
            let image = config.image.clone();
            let result = match permits.acquire_owned().await {
                Ok(_permit) => {
                    runtime
                        .run_container_with_progress(config, operation_tx)
                        .await
                }
                Err(_) => Err(anyhow::anyhow!(
                    "operation manager shut down before operation could start"
                )),
            };
            let _ = forwarder.await;
            let metadata = if result.is_ok() {
                runtime.inspect_image(&image).await.ok()
            } else {
                None
            };
            (id, summary, result, metadata)
        });
        id
    }

    fn drain_updates(&mut self) {
        let mut events = Vec::new();
        if let Some(rx) = &mut self.events_rx {
            while let Ok((id, event)) = rx.try_recv() {
                events.push((id, event));
            }
        }
        for (id, event) in events {
            self.apply_event(id, event);
        }
        while let Some(result) = self.joins.try_join_next() {
            self.handle_join(result);
        }
    }

    async fn wait_all(&mut self) -> Result<()> {
        // Do not await a join in isolation: Docker emits pull events while the
        // task is alive, and those events must be rendered as they arrive.
        while !self.joins.is_empty() {
            tokio::select! {
                Some((id, event)) = async {
                    match &mut self.events_rx {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => self.apply_event(id, event),
                Some(result) = self.joins.join_next() => self.handle_join(result),
            }
        }
        self.drain_updates();
        if self.failures.is_empty() {
            Ok(())
        } else {
            bail!("{} container operation(s) failed", self.failures.len())
        }
    }

    fn apply_event(&mut self, id: u64, event: OperationEvent) {
        // Docker's byte totals are per layer and a layer can finish well
        // before image extraction or the readiness probe. Do not leave a
        // misleading full operation bar on screen after the pull phase.
        if event.status != OperationStatus::Pulling {
            if let Some(progress) = self.byte_progress.remove(&id) {
                progress.finish("");
            }
            self.pull_layers.remove(&id);
            self.displayed_percent.remove(&id);
        }
        if let (Some(current), Some(total)) = (event.current_bytes, event.total_bytes)
            && total > 0
            && current >= 0
        {
            // Docker reports bytes *per layer*, not for the whole image. Keep
            // the latest value for each layer and render their aggregate so a
            // layer switch never resets the operation bar back to zero.
            let layers = self.pull_layers.entry(id).or_default();
            layers.insert(
                event
                    .layer_id
                    .clone()
                    .unwrap_or_else(|| "operation".to_string()),
                (current as u64, total as u64),
            );
            let (aggregate_current, aggregate_total) = layers.values().fold(
                (0_u64, 0_u64),
                |(current_sum, total_sum), (layer_current, layer_total)| {
                    (
                        current_sum.saturating_add(*layer_current),
                        total_sum.saturating_add(*layer_total),
                    )
                },
            );
            let progress = self.byte_progress.entry(id).or_insert_with(|| {
                ui::Progress::new(
                    format!("operation #{id}: downloading image"),
                    aggregate_total,
                )
            });
            let percent = aggregate_current.saturating_mul(100) / aggregate_total.max(1);
            // Layer totals arrive incrementally. A newly discovered layer can
            // make Docker's provisional aggregate smaller even while no bytes
            // were lost, so preserve the highest rendered percentage.
            let displayed = self.displayed_percent.entry(id).or_insert(0);
            // 100% is reserved for the terminal completion state. Docker can
            // report a single layer at 100% while more layers or readiness
            // work remain, so showing it earlier would be false.
            *displayed = (*displayed).max(percent.min(99));
            progress.set_percent(*displayed);
        }
        if let Some(record) = self.records.get_mut(&id) {
            record.status = event.status;
            record.detail = format_progress(&event);
            if event.status == OperationStatus::Failed {
                record.error = Some(event.detail);
            }
        }
    }

    fn handle_join(
        &mut self,
        result: Result<
            (
                u64,
                LaunchSummary,
                Result<crate::docker::ContainerInfo>,
                Option<ImageMetadata>,
            ),
            tokio::task::JoinError,
        >,
    ) {
        match result {
            Ok((id, mut summary, Ok(_), metadata)) => {
                if let Some(progress) = self.byte_progress.remove(&id) {
                    progress.finish(format!("operation #{id}: image ready"));
                }
                self.pull_layers.remove(&id);
                self.displayed_percent.remove(&id);
                if let Some(record) = self.records.get_mut(&id) {
                    record.status = OperationStatus::Running;
                    record.detail = "container is running".into();
                }
                if let Some(metadata) = metadata {
                    summary.resolved_digest = metadata.digests.first().cloned();
                }
                if summary.pin
                    && let Err(error) = pin_summary(&summary)
                {
                    let message = format!(
                        "could not pin image for `{}`: {error:#}",
                        summary.container_name
                    );
                    self.failures.push(message.clone());
                    ui::error(&message);
                }
                let copied = summary
                    .connection_string
                    .as_deref()
                    .is_some_and(copy_to_clipboard);
                print_summary(&summary, copied);
                self.completed_summaries.push(summary);
            }
            Ok((id, _, Err(error), _)) => {
                if let Some(progress) = self.byte_progress.remove(&id) {
                    progress.finish(format!("operation #{id}: image pull failed"));
                }
                self.pull_layers.remove(&id);
                self.displayed_percent.remove(&id);
                if let Some(record) = self.records.get_mut(&id) {
                    record.status = OperationStatus::Failed;
                    record.detail = "operation failed".into();
                    record.error = Some(format!("{error:#}"));
                }
                self.failures.push(format!("operation #{id}: {error:#}"));
                ui::error(&format!("operation #{id} failed: {error:#}"));
            }
            Err(error) => {
                self.failures.push(error.to_string());
                ui::error(&format!("container operation task failed: {error}"));
            }
        }
    }

    fn render_status(&mut self) {
        self.drain_updates();
        println!("\nOperations:");
        for record in self.records.values() {
            let status = format!("{:?}", record.status).to_lowercase();
            let elapsed = record.created_at.elapsed().unwrap_or_default().as_secs();
            println!(
                "  #{:<3} {:<10} {:<14} {:<20} {:<24} {} ({}s)",
                record.id,
                status,
                record.service,
                record.resource,
                record.image,
                record.detail,
                elapsed
            );
            if let Some(error) = &record.error {
                println!("       error: {error}");
            }
        }
        println!();
    }
}

fn format_progress(event: &OperationEvent) -> String {
    match (event.current_bytes, event.total_bytes) {
        (Some(current), Some(total)) if total > 0 => {
            format!("{} ({current}/{total} bytes)", event.detail)
        }
        _ => event.detail.clone(),
    }
}

/// Prompt the user for container customization options.
fn prompt_service_options(
    service: &Service,
    force_advanced: bool,
    docker_ports: &HashMap<u16, String>,
    existing_names: &HashSet<String>,
) -> Result<CustomOptions> {
    let mode = if force_advanced {
        "Custom"
    } else {
        inquire::Select::new(
            &format!("Configuration mode for {}:", service.name),
            vec![
                "Quick   (choose name & port; auto-generate password)",
                "Custom  (customize name, port, password, database, volume)",
            ],
        )
        .with_help_message("Esc: cancel and exit")
        .prompt()
        .map_err(super::prompt_error)?
    };

    let is_custom = mode.starts_with("Custom");

    // 1. Container name (with duplicate validation)
    let default_name = format!("asur-{}", service.name);
    let existing_names_clone = existing_names.clone();
    let container_name = inquire::Text::new("Container name:")
        .with_default(&default_name)
        .with_validator(move |input: &str| {
            let trimmed = input.trim();
            if trimmed.is_empty() {
                Ok(inquire::validator::Validation::Invalid(
                    "Container name cannot be empty".into(),
                ))
            } else if existing_names_clone.contains(trimmed) {
                Ok(inquire::validator::Validation::Invalid(
                    format!(
                        "A container named `{trimmed}` already exists. Please choose a different name."
                    )
                    .into(),
                ))
            } else {
                Ok(inquire::validator::Validation::Valid)
            }
        })
        .prompt()
        .map_err(super::prompt_error)?;

    // 2. Port conflict check & prompt
    let default_port = service.default_port;
    let suggested_port = if is_port_available(default_port, docker_ports) {
        default_port
    } else {
        let next_free = find_available_port(default_port, docker_ports);
        if let Some(owner) = docker_ports.get(&default_port) {
            ui::warn(&format!(
                "Default port {default_port} is already used by container `{owner}`! Suggesting free port {next_free}."
            ));
        } else {
            ui::warn(&format!(
                "Default port {default_port} is already in use by another process! Suggesting free port {next_free}."
            ));
        }
        next_free
    };

    let docker_ports_clone = docker_ports.clone();
    let port = inquire::CustomType::<u16>::new("Host port:")
        .with_default(suggested_port)
        .with_validator(move |&val: &u16| {
            if let Some(owner) = docker_ports_clone.get(&val) {
                Ok(inquire::validator::Validation::Invalid(
                    format!("Port {val} is already used by container `{owner}`. Please pick another port.").into(),
                ))
            } else if !is_port_free(val) {
                Ok(inquire::validator::Validation::Invalid(
                    format!("Port {val} is already in use by another process. Please pick another port.").into(),
                ))
            } else {
                Ok(inquire::validator::Validation::Valid)
            }
        })
        .prompt()
        .map_err(super::prompt_error)?;

    if !is_custom {
        return Ok(CustomOptions {
            container_name: Some(container_name),
            port: Some(port),
            ..Default::default()
        });
    }

    // Custom mode: prompt for password, user, database, volume
    let password = if service.password_env.is_some()
        || service.connection.contains("{password}")
        || service.command.iter().any(|arg| arg.contains("{password}"))
    {
        let input = inquire::Text::new("Password (leave empty to auto-generate):")
            .prompt()
            .map_err(super::prompt_error)?;
        if input.trim().is_empty() {
            None
        } else {
            Some(input.trim().to_string())
        }
    } else {
        None
    };

    let user = if let Some(def_user) = &service.default_user {
        let input = inquire::Text::new("Username:")
            .with_default(def_user)
            .prompt()
            .map_err(super::prompt_error)?;
        Some(input)
    } else {
        None
    };

    let database = if let Some(def_db) = &service.default_database {
        let input = inquire::Text::new("Database name:")
            .with_default(def_db)
            .prompt()
            .map_err(super::prompt_error)?;
        Some(input)
    } else {
        None
    };

    let volume_name = if service.data_path.is_some() {
        let def_vol = format!("{container_name}-data");
        let input = inquire::Text::new("Volume name:")
            .with_default(&def_vol)
            .prompt()
            .map_err(super::prompt_error)?;
        Some(input)
    } else {
        None
    };

    Ok(CustomOptions {
        container_name: Some(container_name),
        port: Some(port),
        user,
        password,
        database,
        volume_name,
    })
}

/// Interactive selection from the catalog when no service is provided on the CLI.
fn select_service_interactive(catalog: &Catalog) -> Result<&Service> {
    let options: Vec<String> = catalog
        .services()
        .iter()
        .map(|s| format!("{:<14} (default port {})", s.name, s.default_port))
        .collect();

    let choice = inquire::Select::new("Choose a service to start:", options)
        .with_help_message("Esc: cancel and exit")
        .prompt()
        .map_err(super::prompt_error)?;

    let chosen_name = choice.split_whitespace().next().unwrap_or_default();
    catalog
        .find(chosen_name)
        .ok_or_else(|| anyhow::anyhow!("service `{chosen_name}` not found in catalog"))
}

/// Resolve a service query by exact match or fuzzy suggestion.
pub fn resolve_service<'a>(
    name: &str,
    catalog: &'a Catalog,
    interactive: bool,
) -> Result<&'a Service> {
    if let Some(service) = catalog.find(name) {
        return Ok(service);
    }

    if let Some(suggested) = catalog.suggest(name) {
        if interactive {
            let prompt_msg = format!(
                "Unknown service `{name}`. Did you mean `{}`?",
                suggested.name
            );
            let confirmed = inquire::Confirm::new(&prompt_msg)
                .with_default(true)
                .prompt()
                .map_err(super::prompt_error)?;

            if confirmed {
                return Ok(suggested);
            }
        }
        bail!(
            "unknown service `{name}`. Did you mean `{}`?",
            suggested.name
        );
    }

    bail!("unknown service `{name}`. Run `asur up` to see available services.");
}

/// Save generated docker-compose.yml to disk.
pub fn write_compose_file(summaries: &[LaunchSummary]) -> Result<()> {
    if summaries.is_empty() {
        ui::warn("No containers were successfully launched, so no docker-compose.yml was created.");
        return Ok(());
    }
    let yaml = generate_compose_yaml(summaries);
    let path = "docker-compose.yml";
    std::fs::write(path, yaml)?;
    ui::info(&format!(
        "Generated `{path}` with {} service(s).",
        summaries.len()
    ));
    Ok(())
}

/// Render a clean summary card of the launched container.
pub fn print_summary(summary: &LaunchSummary, copied_to_clipboard: bool) {
    let green_bold = Style::new().green().bold();
    let label_style = Style::new().bold();
    let yellow_bold = Style::new().yellow().bold();
    let connection_style = Style::new().green().bold();

    let check = "✔".if_supports_color(Stream::Stdout, |t| t.style(green_bold));
    let title = format!("{check} {} is ready!", summary.service_name);
    println!("\n{title}");

    println!(
        "  {}  {}",
        "Container :".if_supports_color(Stream::Stdout, |t| t.style(label_style)),
        summary.container_name
    );
    println!(
        "  {}  {}",
        "Image     :".if_supports_color(Stream::Stdout, |t| t.style(label_style)),
        summary.image
    );
    println!(
        "  {}  {}",
        "Ports     :".if_supports_color(Stream::Stdout, |t| t.style(label_style)),
        if summary.ports.is_empty() {
            "not accessible from this computer (no host port selected)".to_string()
        } else {
            summary
                .ports
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        }
    );

    if let Some(digest) = &summary.resolved_digest {
        println!(
            "  {}  {}",
            "Digest    :".if_supports_color(Stream::Stdout, |t| t.style(label_style)),
            digest
        );
    }

    if let Some(user) = &summary.user {
        println!(
            "  {}  {}",
            "User      :".if_supports_color(Stream::Stdout, |t| t.style(label_style)),
            user
        );
    }

    if let Some(pass) = &summary.password {
        let note = "(save this; shown once)".if_supports_color(Stream::Stdout, |t| t.dimmed());
        println!(
            "  {}  {} {note}",
            "Password  :".if_supports_color(Stream::Stdout, |t| t.style(label_style)),
            pass.if_supports_color(Stream::Stdout, |t| t.style(yellow_bold))
        );
    }

    if let Some(db) = &summary.database {
        println!(
            "  {}  {}",
            "Database  :".if_supports_color(Stream::Stdout, |t| t.style(label_style)),
            db
        );
    }

    if let Some(connection_string) = &summary.connection_string {
        let conn_styled =
            connection_string.if_supports_color(Stream::Stdout, |t| t.style(connection_style));
        println!(
            "  {}  {conn_styled}",
            "Connection:".if_supports_color(Stream::Stdout, |t| t.style(label_style))
        );

        if copied_to_clipboard {
            println!(
                "  {}",
                "✔ Connection string copied to clipboard!"
                    .if_supports_color(Stream::Stdout, |t| t.green())
            );
        }
    }
    println!();
}

fn pin_summary(summary: &LaunchSummary) -> Result<()> {
    let resolved = summary.resolved_digest.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "Docker did not report a repository digest; locally built images cannot be pinned until they are pushed to a registry"
        )
    })?;
    let image = summary.image.parse::<ImageReference>()?;
    let pinned = if resolved.contains('@') {
        resolved.to_string()
    } else {
        image.pinned_with(resolved)
    };
    let path = Path::new(".asur.toml");
    let mut document = if path.exists() {
        let content = std::fs::read_to_string(path).context("could not read `.asur.toml`")?;
        toml::from_str::<toml::Value>(&content).context("existing `.asur.toml` is invalid")?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let root = document
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("`.asur.toml` must contain a TOML table"))?;
    root.entry("version".to_string())
        .or_insert(toml::Value::Integer(1));
    let services = root
        .entry("service".to_string())
        .or_insert_with(|| toml::Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| {
            anyhow::anyhow!("`.asur.toml` field `service` must be an array of tables")
        })?;

    let existing = services.iter_mut().find(|service| {
        service
            .as_table()
            .and_then(|table| table.get("name"))
            .and_then(toml::Value::as_str)
            == Some(summary.container_name.as_str())
    });
    if let Some(service) = existing {
        let table = service
            .as_table_mut()
            .expect("matching service was verified as a table");
        table.insert("image".to_string(), toml::Value::String(pinned.clone()));
    } else {
        let mut service = toml::map::Map::new();
        service.insert(
            "name".to_string(),
            toml::Value::String(summary.container_name.clone()),
        );
        service.insert("image".to_string(), toml::Value::String(pinned.clone()));
        services.push(toml::Value::Table(service));
    }
    let rendered = toml::to_string_pretty(&document).context("could not serialize `.asur.toml`")?;
    std::fs::write(path, rendered).context("could not write `.asur.toml`")?;
    ui::info(&format!("Pinned `{pinned}` in `.asur.toml`."));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::{ContainerInfo, CreateContainerConfig};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TestRuntime {
        run_count: AtomicUsize,
    }

    impl TestRuntime {
        fn new() -> Self {
            Self {
                run_count: AtomicUsize::new(0),
            }
        }
    }

    impl ContainerRuntime for TestRuntime {
        async fn list_containers(&self, _all: bool) -> Result<Vec<ContainerInfo>> {
            Ok(vec![])
        }

        async fn run_container(&self, config: CreateContainerConfig) -> Result<ContainerInfo> {
            self.run_count.fetch_add(1, Ordering::SeqCst);
            Ok(ContainerInfo {
                name: config.name,
                image: config.image,
                status: "Up just now".into(),
                state: "running".into(),
                ports: vec!["0.0.0.0:5432->5432/tcp".into()],
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
                image: "redis:7".to_string(),
                env: vec![],
            })
        }
    }

    #[test]
    fn test_resolve_exact_service() {
        let catalog = Catalog::load().unwrap();
        let service = resolve_service("postgres", &catalog, false).unwrap();
        assert_eq!(service.name, "postgres");
    }

    #[test]
    fn test_resolve_suggests_typo_non_interactive() {
        let catalog = Catalog::load().unwrap();
        let err = resolve_service("reids", &catalog, false).unwrap_err();
        assert!(err.to_string().contains("Did you mean `redis`?"));
    }

    #[test]
    fn test_resolve_unknown_service() {
        let catalog = Catalog::load().unwrap();
        let err = resolve_service("unknown_xyz_123", &catalog, false).unwrap_err();
        assert!(err.to_string().contains("unknown service"));
    }

    #[tokio::test]
    async fn test_runtime_run_container_call() {
        let runtime = TestRuntime::new();
        let config = CreateContainerConfig {
            name: "test-redis".into(),
            image: "redis:7".into(),
            env: vec![],
            ports: vec![PortBinding::tcp(6379, 6379)],
            mounts: vec![],
            network: None,
            restart: RestartPolicy::UnlessStopped,
            pull_policy: PullPolicy::Missing,
            probe_readiness: true,
            command: None,
        };
        let info = runtime.run_container(config).await.unwrap();
        assert_eq!(info.name, "test-redis");
        assert_eq!(runtime.run_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn explicit_env_overrides_env_file() {
        let unique = format!(
            "asur-env-test-{}-{}.env",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::write(&path, "API_URL=file\nFROM_FILE=yes\n").unwrap();
        let values =
            load_environment(std::slice::from_ref(&path), &["API_URL=cli".into()]).unwrap();
        std::fs::remove_file(path).unwrap();
        assert!(values.contains(&"API_URL=cli".to_string()));
        assert!(values.contains(&"FROM_FILE=yes".to_string()));
        assert!(!values.contains(&"API_URL=file".to_string()));
    }

    #[test]
    fn generic_name_validation_rejects_conflicts_and_bad_characters() {
        let existing = HashSet::from(["asur-nginx".to_string()]);
        assert!(validate_container_name("asur-nginx", &existing).is_err());
        assert!(validate_container_name("bad name", &HashSet::new()).is_err());
        assert!(validate_container_name("valid.name-1", &HashSet::new()).is_ok());
    }

    #[test]
    fn stable_tag_picker_prefers_newest_ga_versions() {
        let tags = vec![
            ImageTag {
                name: "31.0.rc25".into(),
                ..Default::default()
            },
            ImageTag {
                name: "31.0.rc25-amd64".into(),
                ..Default::default()
            },
            ImageTag {
                name: "29.1".into(),
                ..Default::default()
            },
            ImageTag {
                name: "30.2".into(),
                ..Default::default()
            },
            ImageTag {
                name: "30.1".into(),
                ..Default::default()
            },
        ];
        assert_eq!(preferred_stable_tags(&tags), vec!["30.2", "30.1", "29.1"]);
    }
}
