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
        out.push_str("    restart: unless-stopped\n");

        if let Some(cmd) = &s.command {
            out.push_str(&format!("    command: {}\n", cmd.join(" ")));
        }

        out.push_str("    ports:\n");
        out.push_str(&format!("      - \"{}:{}\"\n", s.port, s.container_port));

        if !s.env.is_empty() {
            out.push_str("    environment:\n");
            for env in &s.env {
                out.push_str(&format!("      - {env}\n"));
            }
        }

        if !s.volumes.is_empty() {
            out.push_str("    volumes:\n");
            for (vol_name, container_path) in &s.volumes {
                out.push_str(&format!("      - {vol_name}:{container_path}\n"));
                if !all_volumes.contains(vol_name) {
                    all_volumes.push(vol_name.clone());
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

    #[test]
    fn test_generate_compose() {
        let summaries = vec![LaunchSummary {
            service_name: "redis".into(),
            container_name: "my-redis".into(),
            image: "redis:7-alpine".into(),
            port: 6380,
            container_port: 6379,
            user: None,
            password: Some("secret123".into()),
            database: None,
            connection_string: "redis://:secret123@localhost:6380".into(),
            env: vec![],
            volumes: vec![("my-redis-data".into(), "/data".into())],
            command: Some(vec![
                "redis-server".into(),
                "--requirepass".into(),
                "secret123".into(),
            ]),
        }];

        let yaml = generate_compose_yaml(&summaries);
        assert!(yaml.contains("services:"));
        assert!(yaml.contains("image: redis:7-alpine"));
        assert!(yaml.contains("container_name: my-redis"));
        assert!(yaml.contains("6380:6379"));
        assert!(yaml.contains("command: redis-server --requirepass secret123"));
        assert!(yaml.contains("volumes:"));
        assert!(yaml.contains("my-redis-data:/data"));
    }
}
