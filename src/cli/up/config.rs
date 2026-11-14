//! Configuration builder for `asur up`.
//!
//! Pure functions for constructing [`CreateContainerConfig`] and [`LaunchSummary`]
//! without touching Docker.

use crate::catalog::Service;
use crate::docker::{
    CreateContainerConfig, ImageReference, Mount, PortBinding, PullPolicy, RestartPolicy,
};
use crate::util::password::generate_password;

/// User overrides for configuring a service container.
#[derive(Debug, Clone, Default)]
pub struct CustomOptions {
    pub container_name: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub database: Option<String>,
    pub volume_name: Option<String>,
}

/// Summary details for displaying to the user after container startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSummary {
    pub service_name: String,
    pub container_name: String,
    pub image: String,
    pub ports: Vec<PortBinding>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub database: Option<String>,
    pub connection_string: Option<String>,
    pub env: Vec<String>,
    pub mounts: Vec<Mount>,
    pub network: Option<String>,
    pub restart: RestartPolicy,
    pub command: Option<Vec<String>>,
    pub resolved_digest: Option<String>,
    pub pin: bool,
    pub generic: bool,
}

#[derive(Debug, Clone)]
pub struct GenericOptions {
    pub image: ImageReference,
    pub container_name: String,
    pub env: Vec<String>,
    pub ports: Vec<PortBinding>,
    pub mounts: Vec<Mount>,
    pub network: Option<String>,
    pub restart: RestartPolicy,
    pub command: Option<Vec<String>>,
    pub pull_policy: PullPolicy,
    pub resolved_digest: Option<String>,
    pub pin: bool,
}

/// Build container configuration with optional user customizations.
pub fn build_container_config(
    service: &Service,
    options: &CustomOptions,
) -> (CreateContainerConfig, LaunchSummary) {
    let container_name = options
        .container_name
        .clone()
        .unwrap_or_else(|| format!("asur-{}", service.name));

    let port = options.port.unwrap_or(service.default_port);
    let image = format!("{}:{}", service.image, service.default_tag);

    let user = options
        .user
        .clone()
        .or_else(|| service.default_user.clone());

    let database = options
        .database
        .clone()
        .or_else(|| service.default_database.clone());

    let password = options.password.clone().or_else(|| {
        if service.password_env.is_some()
            || service.connection.contains("{password}")
            || service.command.iter().any(|arg| arg.contains("{password}"))
        {
            Some(generate_password(16))
        } else {
            None
        }
    });

    let user_str = user.as_deref().unwrap_or("");
    let pass_str = password.as_deref().unwrap_or("");
    let db_str = database.as_deref().unwrap_or("");

    // Build environment variables
    let mut env = Vec::new();
    if let Some(user_env) = &service.user_env {
        env.push(format!("{user_env}={user_str}"));
    }
    if let Some(pass_env) = &service.password_env {
        env.push(format!("{pass_env}={pass_str}"));
    }
    if let Some(db_env) = &service.database_env {
        env.push(format!("{db_env}={db_str}"));
    }
    for extra in &service.extra_env {
        env.push(extra.clone());
    }

    // Build command override if any
    let command = if service.command.is_empty() {
        None
    } else {
        Some(
            service
                .command
                .iter()
                .map(|arg| {
                    arg.replace("{password}", pass_str)
                        .replace("{user}", user_str)
                        .replace("{port}", &port.to_string())
                        .replace("{database}", db_str)
                })
                .collect(),
        )
    };

    // Named volume for data persistence if data path is defined
    let mut mounts = Vec::new();
    if let Some(data_path) = &service.data_path {
        let vol_name = options
            .volume_name
            .clone()
            .unwrap_or_else(|| format!("asur-{}-data", service.name));
        mounts.push(Mount::volume(vol_name, data_path.clone()));
    }

    // Build connection string
    let connection_string = service
        .connection
        .replace("{user}", user_str)
        .replace("{password}", pass_str)
        .replace("{port}", &port.to_string())
        .replace("{database}", db_str);

    let container_config = CreateContainerConfig {
        name: container_name.clone(),
        image: image.clone(),
        env,
        ports: vec![PortBinding::tcp(port, service.default_port)],
        mounts,
        network: None,
        restart: RestartPolicy::UnlessStopped,
        pull_policy: PullPolicy::Missing,
        probe_readiness: true,
        command,
    };

    let summary = LaunchSummary {
        service_name: service.name.clone(),
        container_name,
        image,
        ports: container_config.ports.clone(),
        user,
        password,
        database,
        connection_string: Some(connection_string),
        env: container_config.env.clone(),
        mounts: container_config.mounts.clone(),
        network: container_config.network.clone(),
        restart: container_config.restart,
        command: container_config.command.clone(),
        resolved_digest: None,
        pin: false,
        generic: false,
    };

    (container_config, summary)
}

pub fn build_generic_config(options: GenericOptions) -> (CreateContainerConfig, LaunchSummary) {
    let image = options.image.to_string();
    let config = CreateContainerConfig {
        name: options.container_name.clone(),
        image: image.clone(),
        env: options.env,
        ports: options.ports,
        mounts: options.mounts,
        network: options.network,
        restart: options.restart,
        pull_policy: options.pull_policy,
        probe_readiness: false,
        command: options.command,
    };
    let summary = LaunchSummary {
        service_name: options
            .image
            .repository()
            .rsplit('/')
            .next()
            .unwrap_or("image")
            .to_string(),
        container_name: config.name.clone(),
        image,
        ports: config.ports.clone(),
        user: None,
        password: None,
        database: None,
        connection_string: None,
        env: config.env.clone(),
        mounts: config.mounts.clone(),
        network: config.network.clone(),
        restart: config.restart,
        command: config.command.clone(),
        resolved_digest: options.resolved_digest,
        pin: options.pin,
        generic: true,
    };
    (config, summary)
}

/// Build container configuration and connection details for quick mode.
pub fn build_quick_config(service: &Service) -> (CreateContainerConfig, LaunchSummary) {
    build_container_config(service, &CustomOptions::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;

    #[test]
    fn test_postgres_quick_config() {
        let catalog = Catalog::load().unwrap();
        let pg = catalog.find("postgres").unwrap();
        let (config, summary) = build_quick_config(pg);

        assert_eq!(config.name, "asur-postgres");
        assert_eq!(config.ports, vec![PortBinding::tcp(5432, 5432)]);
        assert!(config.image.starts_with("postgres:"));
        assert!(summary.password.is_some());
        let pass = summary.password.unwrap();
        assert_eq!(pass.len(), 16);
        assert!(config.env.contains(&format!("POSTGRES_PASSWORD={pass}")));
        assert!(config.env.contains(&"POSTGRES_USER=postgres".to_string()));
        assert!(summary.connection_string.as_ref().unwrap().contains(&pass));
        assert!(
            summary
                .connection_string
                .as_ref()
                .unwrap()
                .starts_with("postgres://postgres:")
        );
        assert_eq!(config.mounts.len(), 1);
        assert_eq!(config.mounts[0].source, "asur-postgres-data");
    }

    #[test]
    fn test_custom_options() {
        let catalog = Catalog::load().unwrap();
        let redis = catalog.find("redis").unwrap();
        let custom = CustomOptions {
            container_name: Some("my-custom-redis".into()),
            port: Some(6380),
            password: Some("mypassword".into()),
            ..Default::default()
        };
        let (config, summary) = build_container_config(redis, &custom);

        assert_eq!(config.name, "my-custom-redis");
        assert_eq!(config.ports, vec![PortBinding::tcp(6380, 6379)]);
        assert_eq!(summary.password, Some("mypassword".into()));
        assert!(
            summary
                .connection_string
                .as_ref()
                .unwrap()
                .contains("localhost:6380")
        );
        assert!(
            summary
                .connection_string
                .as_ref()
                .unwrap()
                .contains(":mypassword@")
        );
    }

    #[test]
    fn generic_config_preserves_all_runtime_controls() {
        let options = GenericOptions {
            image: "ghcr.io/acme/api:1.4".parse().unwrap(),
            container_name: "api".into(),
            env: vec!["MODE=test".into()],
            ports: vec![
                "127.0.0.1:8080:80/tcp".parse().unwrap(),
                "5353:53/udp".parse().unwrap(),
            ],
            mounts: vec!["cache:/cache:ro".parse().unwrap()],
            network: Some("devnet".into()),
            restart: RestartPolicy::OnFailure {
                maximum_retry_count: 3,
            },
            command: Some(vec!["serve".into(), "--debug".into()]),
            pull_policy: PullPolicy::Always,
            resolved_digest: Some(format!("ghcr.io/acme/api@sha256:{}", "a".repeat(64))),
            pin: true,
        };
        let (config, summary) = build_generic_config(options);
        assert_eq!(config.ports.len(), 2);
        assert!(config.mounts[0].read_only);
        assert_eq!(config.network.as_deref(), Some("devnet"));
        assert_eq!(
            config.restart,
            RestartPolicy::OnFailure {
                maximum_retry_count: 3
            }
        );
        assert_eq!(config.pull_policy, PullPolicy::Always);
        assert!(!config.probe_readiness);
        assert!(summary.pin);
        assert!(summary.generic);
    }
}
