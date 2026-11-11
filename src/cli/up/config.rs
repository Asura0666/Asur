//! Configuration builder for `asur up`.
//!
//! Pure functions for constructing [`CreateContainerConfig`] and [`LaunchSummary`]
//! without touching Docker.

use crate::catalog::Service;
use crate::docker::CreateContainerConfig;
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
    pub port: u16,
    pub container_port: u16,
    pub user: Option<String>,
    pub password: Option<String>,
    pub database: Option<String>,
    pub connection_string: String,
    pub env: Vec<String>,
    pub volumes: Vec<(String, String)>,
    pub command: Option<Vec<String>>,
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
    let mut volumes = Vec::new();
    if let Some(data_path) = &service.data_path {
        let vol_name = options
            .volume_name
            .clone()
            .unwrap_or_else(|| format!("asur-{}-data", service.name));
        volumes.push((vol_name, data_path.clone()));
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
        ports: vec![(port, service.default_port)],
        volumes,
        command,
    };

    let summary = LaunchSummary {
        service_name: service.name.clone(),
        container_name,
        image,
        port,
        container_port: service.default_port,
        user,
        password,
        database,
        connection_string,
        env: container_config.env.clone(),
        volumes: container_config.volumes.clone(),
        command: container_config.command.clone(),
    };

    (container_config, summary)
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
        assert_eq!(config.ports, vec![(5432, 5432)]);
        assert!(config.image.starts_with("postgres:"));
        assert!(summary.password.is_some());
        let pass = summary.password.unwrap();
        assert_eq!(pass.len(), 16);
        assert!(config.env.contains(&format!("POSTGRES_PASSWORD={pass}")));
        assert!(config.env.contains(&"POSTGRES_USER=postgres".to_string()));
        assert!(summary.connection_string.contains(&pass));
        assert!(
            summary
                .connection_string
                .starts_with("postgres://postgres:")
        );
        assert_eq!(config.volumes.len(), 1);
        assert_eq!(config.volumes[0].0, "asur-postgres-data");
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
        assert_eq!(config.ports, vec![(6380, 6379)]);
        assert_eq!(summary.port, 6380);
        assert_eq!(summary.password, Some("mypassword".into()));
        assert!(summary.connection_string.contains("localhost:6380"));
        assert!(summary.connection_string.contains(":mypassword@"));
    }
}
