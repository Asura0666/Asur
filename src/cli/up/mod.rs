//! Container spin-up wizard (`asur up`).

mod compose;
mod config;

pub use compose::generate_compose_yaml;
pub use config::{CustomOptions, LaunchSummary, build_container_config};

use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::sync::Arc;
use std::time::SystemTime;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use anyhow::{Result, bail};
use clap::Args;
use owo_colors::{OwoColorize, Stream, Style};

use crate::catalog::{Catalog, Service};
use crate::docker::{
    ContainerRuntime, DockerRuntime, OperationEvent, OperationRecord, OperationStatus,
};
use crate::ui;
use crate::util::clipboard::copy_to_clipboard;
use crate::util::ports::{
    extract_docker_ports, find_available_port, is_port_available, is_port_free,
};

#[derive(Args, Debug, Clone, Default)]
pub struct UpArgs {
    /// Services to start, e.g. `postgres redis` (omit to pick from an interactive list)
    pub services: Vec<String>,
    /// Skip questions: use defaults and auto-generated credentials
    #[arg(short, long)]
    pub quick: bool,
    /// Show advanced options (custom port, password, volume, etc.)
    #[arg(long)]
    pub advanced: bool,
    /// Generate a docker-compose.yml from the chosen services
    #[arg(long)]
    pub compose: bool,
}

pub async fn run(args: UpArgs) -> Result<()> {
    let catalog = Catalog::load()?;
    let runtime = Arc::new(DockerRuntime::connect()?);
    let is_interactive = std::io::stdin().is_terminal();
    let mut operations = OperationManager::default();
    let mut initial_services = args.services.clone();

    loop {
        // Query existing containers from Docker to discover already allocated ports & names
        operations.drain_updates();
        let existing_containers = runtime.list_containers(true).await.unwrap_or_default();
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
        let service = if !initial_services.is_empty() {
            let name = initial_services.remove(0);
            resolve_service(&name, &catalog, is_interactive)?
        } else if is_interactive {
            select_service_interactive(&catalog)?
        } else {
            bail!("no service specified. Run `asur up <service>` (e.g. `asur up redis`).");
        };

        // Determine options (quick vs custom)
        let options = if args.quick {
            CustomOptions::default()
        } else if is_interactive {
            prompt_service_options(service, args.advanced, &docker_ports, &existing_names)?
        } else {
            CustomOptions::default()
        };

        let (container_config, summary) = build_container_config(service, &options);

        let operation_id = operations.start(runtime.clone(), container_config, summary);
        ui::info(&format!(
            "Queued operation #{operation_id}. Continue configuring services while it runs."
        ));

        // If command-line specified multiple services, continue through them
        if !initial_services.is_empty() {
            continue;
        }

        // Interactive follow-up options: done, compose, or spin another
        if is_interactive {
            let actions = vec![
                "Done (wait for operations and exit)",
                "Spin up another service",
                "Show operation status",
                "Generate docker-compose.yml (wait for operations)",
            ];

            let action = inquire::Select::new("What would you like to do next?", actions)
                .prompt()
                .unwrap_or("Done (exit)");

            match action {
                "Spin up another service" => continue,
                "Show operation status" => {
                    operations.render_status();
                    continue;
                }
                "Generate docker-compose.yml (wait for operations)" => {
                    operations.wait_all().await;
                    write_compose_file(&operations.completed_summaries)?;
                    break;
                }
                _ => break,
            }
        } else {
            break;
        }
    }

    operations.wait_all().await;
    if args.compose && !operations.completed_summaries.is_empty() {
        write_compose_file(&operations.completed_summaries)?;
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
    joins: JoinSet<(u64, LaunchSummary, Result<crate::docker::ContainerInfo>)>,
    events_tx: Option<tokio::sync::mpsc::UnboundedSender<(u64, OperationEvent)>>,
    events_rx: Option<tokio::sync::mpsc::UnboundedReceiver<(u64, OperationEvent)>>,
    byte_progress: HashMap<u64, ui::Progress>,
    pull_layers: HashMap<u64, HashMap<String, (u64, u64)>>,
    displayed_percent: HashMap<u64, u64>,
    completed_summaries: Vec<LaunchSummary>,
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
            .extend(config.ports.iter().map(|(host, _)| *host));
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
            (id, summary, result)
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

    async fn wait_all(&mut self) {
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
            (u64, LaunchSummary, Result<crate::docker::ContainerInfo>),
            tokio::task::JoinError,
        >,
    ) {
        match result {
            Ok((id, summary, Ok(_))) => {
                if let Some(progress) = self.byte_progress.remove(&id) {
                    progress.finish(format!("operation #{id}: image ready"));
                }
                self.pull_layers.remove(&id);
                self.displayed_percent.remove(&id);
                if let Some(record) = self.records.get_mut(&id) {
                    record.status = OperationStatus::Running;
                    record.detail = "container is running".into();
                }
                let copied = copy_to_clipboard(&summary.connection_string);
                print_summary(&summary, copied);
                self.completed_summaries.push(summary);
            }
            Ok((id, _, Err(error))) => {
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
                ui::error(&format!("operation #{id} failed: {error:#}"));
            }
            Err(error) => ui::error(&format!("container operation task failed: {error}")),
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
        .prompt()
        .unwrap_or("Quick")
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
        .unwrap_or(default_name);

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
        .unwrap_or(suggested_port);

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
            .unwrap_or_default();
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
            .unwrap_or_else(|_| def_user.clone());
        Some(input)
    } else {
        None
    };

    let database = if let Some(def_db) = &service.default_database {
        let input = inquire::Text::new("Database name:")
            .with_default(def_db)
            .prompt()
            .unwrap_or_else(|_| def_db.clone());
        Some(input)
    } else {
        None
    };

    let volume_name = if service.data_path.is_some() {
        let def_vol = format!("{container_name}-data");
        let input = inquire::Text::new("Volume name:")
            .with_default(&def_vol)
            .prompt()
            .unwrap_or(def_vol);
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
        .prompt()
        .map_err(|e| anyhow::anyhow!("selection cancelled: {e}"))?;

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
                .unwrap_or(false);

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
    let cyan_bold = Style::new().cyan().bold();

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
        "Port      :".if_supports_color(Stream::Stdout, |t| t.style(label_style)),
        summary.port
    );

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

    let conn_styled = summary
        .connection_string
        .if_supports_color(Stream::Stdout, |t| t.style(cyan_bold));
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
    println!();
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
            ports: vec![(6379, 6379)],
            volumes: vec![],
            command: None,
        };
        let info = runtime.run_container(config).await.unwrap();
        assert_eq!(info.name, "test-redis");
        assert_eq!(runtime.run_count.load(Ordering::SeqCst), 1);
    }
}
