//! Safe, deliberately conservative Docker Compose import.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_yaml::Value;

use super::{HealthCheck, ProfileConfig, ServiceConfig, sanitize_name};
use crate::catalog::Catalog;

#[derive(Debug, Clone, Default)]
pub struct ComposeImport {
    pub services: BTreeMap<String, ServiceConfig>,
    pub profiles: BTreeMap<String, ProfileConfig>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ComposeFile {
    #[serde(default)]
    services: BTreeMap<String, ComposeService>,
}

#[derive(Debug, Default, Deserialize)]
struct ComposeService {
    image: Option<String>,
    container_name: Option<String>,
    #[serde(default)]
    ports: Vec<Value>,
    #[serde(default)]
    volumes: Vec<Value>,
    environment: Option<Value>,
    depends_on: Option<Value>,
    #[serde(default)]
    profiles: Vec<String>,
    command: Option<Value>,
    restart: Option<String>,
    network_mode: Option<String>,
    healthcheck: Option<ComposeHealthcheck>,
}

#[derive(Debug, Default, Deserialize)]
struct ComposeHealthcheck {
    test: Option<Value>,
    interval: Option<String>,
    timeout: Option<String>,
    retries: Option<u64>,
    start_period: Option<String>,
    disable: Option<bool>,
}

pub fn import_compose(path: &Path, catalog: &Catalog) -> Result<ComposeImport> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("could not read Compose file `{}`", path.display()))?;
    let compose: ComposeFile = serde_yaml::from_str(&content)
        .with_context(|| format!("Compose file `{}` is invalid", path.display()))?;
    let mut imported = ComposeImport::default();

    for (raw_name, compose_service) in compose.services {
        let name = sanitize_name(&raw_name);
        if name.is_empty() {
            imported.warnings.push(format!(
                "skipped Compose service with invalid name `{raw_name}`"
            ));
            continue;
        }
        let Some(image) = compose_service.image.clone() else {
            imported.warnings.push(format!(
                "skipped Compose service `{raw_name}` because build-only services need an explicit image before Asur can reproduce them"
            ));
            continue;
        };

        let preset = catalog
            .services()
            .iter()
            .find(|service| image == format!("{}:{}", service.image, service.default_tag));
        let mut service = ServiceConfig {
            preset: preset.map(|service| service.name.clone()),
            image: preset.is_none().then_some(image),
            container_name: compose_service.container_name,
            network: compose_service.network_mode,
            ..Default::default()
        };

        for port in compose_service.ports {
            match short_string(&port).and_then(|value| compose_port(&value)) {
                Some(Ok(binding)) => service.ports.push(binding),
                Some(Err(error)) => imported
                    .warnings
                    .push(format!("service `{name}` port was skipped: {error:#}")),
                None => imported.warnings.push(format!(
                    "service `{name}` uses unsupported long-form Compose port syntax"
                )),
            }
        }
        for mount in compose_service.volumes {
            match short_string(&mount).and_then(|value| value.parse().ok()) {
                Some(mount) => service.mounts.push(mount),
                None => imported.warnings.push(format!(
                    "service `{name}` uses an unsupported or anonymous volume; add an explicit source in .asur.toml"
                )),
            }
        }

        service.environment = compose_environment(
            compose_service.environment.as_ref(),
            &name,
            &mut imported.warnings,
        );
        service.depends_on = compose_dependencies(compose_service.depends_on.as_ref());
        service.command = compose_service
            .command
            .as_ref()
            .map(compose_command)
            .unwrap_or_default();
        if let Some(restart) = compose_service.restart {
            match restart.parse() {
                Ok(policy) => service.restart = Some(policy),
                Err(error) => imported.warnings.push(format!(
                    "service `{name}` restart policy was skipped: {error:#}"
                )),
            }
        }
        service.health = compose_healthcheck(compose_service.healthcheck.as_ref());

        for profile in compose_service.profiles {
            let profile = sanitize_name(&profile);
            if !profile.is_empty() {
                imported
                    .profiles
                    .entry(profile)
                    .or_default()
                    .services
                    .push(name.clone());
            }
        }
        imported.services.insert(name, service);
    }

    if !imported.services.is_empty() {
        imported.profiles.insert(
            "full".to_string(),
            ProfileConfig {
                description: "All project services".to_string(),
                services: imported.services.keys().cloned().collect(),
            },
        );
    }
    let imported_names: BTreeSet<_> = imported.services.keys().cloned().collect();
    for (name, service) in &mut imported.services {
        service.depends_on.retain(|dependency| {
            let keep = imported_names.contains(dependency);
            if !keep {
                imported.warnings.push(format!(
                    "service `{name}` dependency `{dependency}` was skipped because it was not importable"
                ));
            }
            keep
        });
    }
    for profile in imported.profiles.values_mut() {
        profile.services.sort();
        profile.services.dedup();
    }
    Ok(imported)
}

fn short_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn compose_port(value: &str) -> Option<Result<crate::docker::PortBinding>> {
    if !value.contains(':') {
        let (port, protocol) = value.split_once('/').unwrap_or((value, "tcp"));
        port.parse::<u16>().ok()?;
        return Some(format!("{port}:{port}/{protocol}").parse());
    }
    Some(value.parse())
}

fn compose_environment(
    value: Option<&Value>,
    service: &str,
    warnings: &mut Vec<String>,
) -> BTreeMap<String, String> {
    let mut environment = BTreeMap::new();
    let Some(value) = value else {
        return environment;
    };
    match value {
        Value::Sequence(items) => {
            for item in items {
                let Some(item) = short_string(item) else {
                    continue;
                };
                let (key, raw_value) = item.split_once('=').unwrap_or((&item, ""));
                insert_env_reference(&mut environment, key, raw_value, service, warnings);
            }
        }
        Value::Mapping(items) => {
            for (key, value) in items {
                let Some(key) = short_string(key) else {
                    continue;
                };
                let raw_value = short_string(value).unwrap_or_default();
                insert_env_reference(&mut environment, &key, &raw_value, service, warnings);
            }
        }
        _ => warnings.push(format!(
            "service `{service}` has unsupported Compose environment syntax"
        )),
    }
    environment
}

fn insert_env_reference(
    environment: &mut BTreeMap<String, String>,
    key: &str,
    raw_value: &str,
    service: &str,
    warnings: &mut Vec<String>,
) {
    let source = compose_variable(raw_value).unwrap_or(key);
    environment.insert(key.to_string(), source.to_string());
    if !raw_value.is_empty() && source == key && !raw_value.starts_with('$') {
        warnings.push(format!(
            "service `{service}` environment value for `{key}` was not copied; put it in a project env file"
        ));
    }
}

fn compose_variable(value: &str) -> Option<&str> {
    let value = value.strip_prefix("${")?;
    let value = value.strip_suffix('}')?;
    Some(
        value
            .split_once([':', '-', '?'])
            .map_or(value, |(key, _)| key),
    )
}

fn compose_dependencies(value: Option<&Value>) -> Vec<String> {
    let mut dependencies = BTreeSet::new();
    match value {
        Some(Value::Sequence(items)) => {
            dependencies.extend(
                items
                    .iter()
                    .filter_map(short_string)
                    .map(|name| sanitize_name(&name)),
            );
        }
        Some(Value::Mapping(items)) => {
            dependencies.extend(
                items
                    .keys()
                    .filter_map(short_string)
                    .map(|name| sanitize_name(&name)),
            );
        }
        _ => {}
    }
    dependencies
        .into_iter()
        .filter(|name| !name.is_empty())
        .collect()
}

fn compose_command(value: &Value) -> Vec<String> {
    match value {
        Value::Sequence(items) => items.iter().filter_map(short_string).collect(),
        Value::String(command) => vec!["sh".into(), "-c".into(), command.clone()],
        _ => Vec::new(),
    }
}

fn compose_healthcheck(value: Option<&ComposeHealthcheck>) -> Option<HealthCheck> {
    let value = value?;
    if value.disable == Some(true) {
        return None;
    }
    let mut command = value.test.as_ref().map(compose_command)?;
    if command.first().is_some_and(|item| item == "CMD") {
        command.remove(0);
    } else if command.first().is_some_and(|item| item == "CMD-SHELL") {
        command.remove(0);
        command = vec!["sh".into(), "-c".into(), command.join(" ")];
    }
    if command.is_empty() {
        return None;
    }
    Some(HealthCheck::Command {
        command,
        interval_seconds: value
            .interval
            .as_deref()
            .and_then(parse_duration)
            .unwrap_or(5),
        timeout_seconds: value
            .timeout
            .as_deref()
            .and_then(parse_duration)
            .unwrap_or(3),
        retries: value.retries.unwrap_or(5),
        start_period_seconds: value
            .start_period
            .as_deref()
            .and_then(parse_duration)
            .unwrap_or(0),
    })
}

fn parse_duration(value: &str) -> Option<u64> {
    if let Some(seconds) = value.strip_suffix('s') {
        seconds.parse().ok()
    } else if let Some(minutes) = value.strip_suffix('m') {
        minutes.parse::<u64>().ok()?.checked_mul(60)
    } else {
        value.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_import_keeps_references_not_values() {
        let yaml = r#"
services:
  db:
    image: postgres:16-alpine
    ports: ["5433:5432"]
    environment:
      POSTGRES_PASSWORD: ${DB_PASSWORD}
      POSTGRES_DB: app
  api:
    image: ghcr.io/acme/api:1.4
    depends_on: [db]
    profiles: [full]
"#;
        let path = std::env::temp_dir().join(format!("asur-compose-{}.yml", std::process::id()));
        std::fs::write(&path, yaml).unwrap();
        let imported = import_compose(&path, &Catalog::load().unwrap()).unwrap();
        std::fs::remove_file(path).unwrap();
        let db = &imported.services["db"];
        assert_eq!(db.preset.as_deref(), Some("postgres"));
        assert_eq!(db.environment["POSTGRES_PASSWORD"], "DB_PASSWORD");
        assert_eq!(db.environment["POSTGRES_DB"], "POSTGRES_DB");
        assert!(!format!("{imported:?}").contains("POSTGRES_DB: app"));
        assert_eq!(imported.services["api"].depends_on, ["db"]);
    }
}
