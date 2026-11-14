//! Jump into psql, redis-cli, mysql or mongosh for a database container (`asur db`).

use std::collections::HashMap;
use std::io::IsTerminal;
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::docker::{ContainerInfo, ContainerRuntime, DockerRuntime};
use crate::ui;

#[derive(Args, Debug, Clone, Default)]
pub struct DbArgs {
    /// Database container name or id (omit to pick from a list)
    pub container: Option<String>,
}

/// Configuration for launching a database client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbClientConfig {
    pub command: Vec<String>,
    pub extra_env: Vec<(String, String)>,
}

pub async fn run(args: DbArgs) -> Result<()> {
    let runtime = DockerRuntime::connect()?;
    execute(args, &runtime).await
}

pub async fn execute<R: ContainerRuntime>(args: DbArgs, runtime: &R) -> Result<()> {
    let is_interactive = std::io::stdin().is_terminal();
    let all = runtime.list_containers(false).await?; // running containers only

    if all.is_empty() {
        ui::info("No running containers found.");
        return Ok(());
    }

    let db_candidates: Vec<ContainerInfo> = all
        .into_iter()
        .filter(|c| is_database_image(&c.image))
        .collect();

    if db_candidates.is_empty() {
        ui::info(
            "No running database containers found (e.g. postgres, redis, mysql, mariadb, mongo).",
        );
        return Ok(());
    }

    let target = if let Some(c) = args.container {
        c
    } else if is_interactive {
        select_db_container_interactive(&db_candidates)?
    } else {
        ui::info(
            "No container specified. Run `asur db <container>` or run in an interactive terminal.",
        );
        return Ok(());
    };

    // Inspect container details to retrieve image and environment variables
    let details = runtime.get_container_details(&target).await?;
    let env_map = parse_env_map(&details.env);
    let config = build_db_client_command(&details.image, &env_map)?;

    ui::info(&format!(
        "Opening database client for `{}` ({})...",
        details.name, details.image
    ));

    let mut command = Command::new("docker");
    command.arg("exec").arg("-it");

    for (k, v) in config.extra_env {
        command.arg("-e").arg(format!("{k}={v}"));
    }

    command.arg(&target);
    for arg in config.command {
        command.arg(arg);
    }

    let status = command
        .status()
        .context("failed to launch database client inside container")?;

    if !status.success() && status.code() != Some(130) {
        ui::warn(&format!(
            "Database client exited with code {:?}",
            status.code()
        ));
    }

    Ok(())
}

/// Check if an image name resembles a supported database.
pub fn is_database_image(image: &str) -> bool {
    let lower = image.to_lowercase();
    lower.contains("postgres")
        || lower.contains("redis")
        || lower.contains("valkey")
        || lower.contains("mysql")
        || lower.contains("mariadb")
        || lower.contains("mongo")
}

/// Parse environment variables into a key-value map.
pub fn parse_env_map(env: &[String]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for entry in env {
        if let Some((k, v)) = entry.split_once('=') {
            map.insert(k.to_string(), v.to_string());
        }
    }
    map
}

/// Build the CLI client command and extra environment variables for the detected database.
pub fn build_db_client_command(
    image: &str,
    env: &HashMap<String, String>,
) -> Result<DbClientConfig> {
    let lower = image.to_lowercase();

    if lower.contains("postgres") {
        let user = env
            .get("POSTGRES_USER")
            .cloned()
            .unwrap_or_else(|| "postgres".to_string());
        let db = env
            .get("POSTGRES_DB")
            .cloned()
            .unwrap_or_else(|| user.clone());
        let pass = env.get("POSTGRES_PASSWORD").cloned();

        let mut extra_env = Vec::new();
        if let Some(p) = pass {
            extra_env.push(("PGPASSWORD".to_string(), p));
        }

        let command = vec![
            "psql".to_string(),
            "-U".to_string(),
            user,
            "-d".to_string(),
            db,
        ];
        Ok(DbClientConfig { command, extra_env })
    } else if lower.contains("redis") || lower.contains("valkey") {
        let mut command = vec!["redis-cli".to_string()];
        if let Some(pass) = env.get("REDIS_PASSWORD") {
            command.push("-a".to_string());
            command.push(pass.clone());
        }
        Ok(DbClientConfig {
            command,
            extra_env: Vec::new(),
        })
    } else if lower.contains("mysql") || lower.contains("mariadb") {
        let user = env
            .get("MYSQL_USER")
            .cloned()
            .unwrap_or_else(|| "root".to_string());
        let pass = env
            .get("MYSQL_ROOT_PASSWORD")
            .or_else(|| env.get("MYSQL_PASSWORD"))
            .or_else(|| env.get("MARIADB_ROOT_PASSWORD"));

        let mut command = vec!["mysql".to_string(), "-u".to_string(), user];
        if let Some(p) = pass {
            command.push(format!("-p{p}"));
        }
        if let Some(db) = env
            .get("MYSQL_DATABASE")
            .or_else(|| env.get("MARIADB_DATABASE"))
        {
            command.push(db.clone());
        }
        Ok(DbClientConfig {
            command,
            extra_env: Vec::new(),
        })
    } else if lower.contains("mongo") {
        Ok(DbClientConfig {
            command: vec!["mongosh".to_string()],
            extra_env: Vec::new(),
        })
    } else {
        bail!("unsupported database image `{image}`");
    }
}

fn select_db_container_interactive(containers: &[ContainerInfo]) -> Result<String> {
    let options: Vec<String> = containers
        .iter()
        .map(|c| format!("{:<20} {:<24} ({})", c.name, c.image, c.status))
        .collect();

    let choice = inquire::Select::new("Select a database container to connect to:", options)
        .with_help_message("Esc: cancel and exit")
        .prompt()
        .map_err(super::prompt_error)?;

    let name = choice
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("invalid selection"))?;

    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_database_image() {
        assert!(is_database_image("postgres:16-alpine"));
        assert!(is_database_image("redis:7-alpine"));
        assert!(is_database_image("mysql:8.4"));
        assert!(is_database_image("mariadb:11"));
        assert!(is_database_image("mongo:7"));
        assert!(!is_database_image("nginx:latest"));
        assert!(!is_database_image("node:20-alpine"));
    }

    #[test]
    fn test_postgres_client_command() {
        let mut env = HashMap::new();
        env.insert("POSTGRES_USER".to_string(), "root".to_string());
        env.insert("POSTGRES_PASSWORD".to_string(), "secretpass".to_string());
        env.insert("POSTGRES_DB".to_string(), "mydb".to_string());

        let config = build_db_client_command("postgres:16-alpine", &env).unwrap();
        assert_eq!(config.command, vec!["psql", "-U", "root", "-d", "mydb"]);
        assert_eq!(
            config.extra_env,
            vec![("PGPASSWORD".to_string(), "secretpass".to_string())]
        );
    }

    #[test]
    fn test_redis_client_command() {
        let mut env = HashMap::new();
        env.insert("REDIS_PASSWORD".to_string(), "secret123".to_string());

        let config = build_db_client_command("redis:7-alpine", &env).unwrap();
        assert_eq!(config.command, vec!["redis-cli", "-a", "secret123"]);
        assert!(config.extra_env.is_empty());
    }
}
