//! Generator for docker-compose.yml files from launched service configurations.

use super::LaunchSummary;

/// Generate valid docker-compose.yml content from a list of launch summaries.
pub fn generate_compose_yaml(services: &[LaunchSummary]) -> String {
    let mut out = String::from("services:\n");
    let mut all_volumes = Vec::new();

    for s in services {
        out.push_str(&format!("  {}:\n", s.service_name));
        out.push_str(&format!("    image: {}\n", s.image));
        out.push_str(&format!("    container_name: {}\n", s.container_name));
        out.push_str(&format!("    restart: {}\n", s.restart));

        if let Some(network) = &s.network {
            out.push_str(&format!("    network_mode: {network}\n"));
        }

        if let Some(cmd) = &s.command {
            out.push_str(&format!("    command: {}\n", cmd.join(" ")));
        }

        if !s.ports.is_empty() {
            out.push_str("    ports:\n");
            for port in &s.ports {
                out.push_str(&format!("      - \"{port}\"\n"));
            }
        }

        if !s.env.is_empty() {
            out.push_str("    environment:\n");
            for env in &s.env {
                out.push_str(&format!("      - {env}\n"));
            }
        }

        if !s.mounts.is_empty() {
            out.push_str("    volumes:\n");
            for mount in &s.mounts {
                out.push_str(&format!("      - {mount}\n"));
                if mount.kind == crate::docker::MountKind::Volume
                    && !all_volumes.contains(&mount.source)
                {
                    all_volumes.push(mount.source.clone());
                }
            }
        }

        out.push('\n');
    }

    if !all_volumes.is_empty() {
        out.push_str("volumes:\n");
        for vol in all_volumes {
            out.push_str(&format!("  {vol}:\n"));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::{Mount, PortBinding, RestartPolicy};

    #[test]
    fn test_generate_compose() {
        let summaries = vec![LaunchSummary {
            service_name: "redis".into(),
            container_name: "my-redis".into(),
            image: "redis:7-alpine".into(),
            ports: vec![PortBinding::tcp(6380, 6379)],
            user: None,
            password: Some("secret123".into()),
            database: None,
            connection_string: Some("redis://:secret123@localhost:6380".into()),
            env: vec![],
            mounts: vec![Mount::volume("my-redis-data", "/data")],
            network: None,
            restart: RestartPolicy::UnlessStopped,
            command: Some(vec![
                "redis-server".into(),
                "--requirepass".into(),
                "secret123".into(),
            ]),
            resolved_digest: None,
            pin: false,
            generic: false,
        }];

        let yaml = generate_compose_yaml(&summaries);
        assert!(yaml.contains("services:"));
        assert!(yaml.contains("image: redis:7-alpine"));
        assert!(yaml.contains("container_name: my-redis"));
        assert!(yaml.contains("6380:6379/tcp"));
        assert!(yaml.contains("command: redis-server --requirepass secret123"));
        assert!(yaml.contains("volumes:"));
        assert!(yaml.contains("my-redis-data:/data"));
    }
}
