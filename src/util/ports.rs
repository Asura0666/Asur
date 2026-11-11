//! Port inspection utilities.

use std::collections::HashMap;
use std::net::TcpListener;

use crate::docker::ContainerInfo;

/// Check if a TCP port is available on localhost and 0.0.0.0 via OS socket.
pub fn is_port_free(port: u16) -> bool {
    // Both 0.0.0.0 and 127.0.0.1 must be bindable
    TcpListener::bind(("0.0.0.0", port)).is_ok() && TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Parse a host port from a port string like `"0.0.0.0:6379->6379/tcp"`.
pub fn parse_host_port(port_str: &str) -> Option<u16> {
    if let Some((host_part, _)) = port_str.split_once("->") {
        let port_part = host_part.rsplit(':').next()?;
        port_part.parse::<u16>().ok()
    } else {
        None
    }
}

/// Extract all mapped host ports and their owning container names from a list of containers.
pub fn extract_docker_ports(containers: &[ContainerInfo]) -> HashMap<u16, String> {
    let mut map = HashMap::new();
    for c in containers {
        for p in &c.ports {
            if let Some(port) = parse_host_port(p) {
                map.insert(port, c.name.clone());
            }
        }
    }
    map
}

/// Check if a port is available (both not in active Docker containers and free on OS socket).
pub fn is_port_available(port: u16, docker_ports: &HashMap<u16, String>) -> bool {
    !docker_ports.contains_key(&port) && is_port_free(port)
}

/// Find the next available TCP port starting at `start`.
pub fn find_available_port(start: u16, docker_ports: &HashMap<u16, String>) -> u16 {
    let mut port = start;
    while port < 65535 {
        if is_port_available(port, docker_ports) {
            return port;
        }
        port = port.saturating_add(1);
    }
    start
}

/// Find the next free TCP port starting at `start` (OS socket only).
#[allow(dead_code)]
pub fn find_free_port(start: u16) -> u16 {
    find_available_port(start, &HashMap::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_host_port() {
        assert_eq!(parse_host_port("0.0.0.0:6379->6379/tcp"), Some(6379));
        assert_eq!(parse_host_port(":::5432->5432/tcp"), Some(5432));
        assert_eq!(parse_host_port("8080->80/tcp"), Some(8080));
        assert_eq!(parse_host_port("6379/tcp"), None);
    }

    #[test]
    fn test_extract_docker_ports() {
        let containers = vec![ContainerInfo {
            name: "medusa_redis".into(),
            image: "redis:7".into(),
            status: "Up".into(),
            state: "running".into(),
            ports: vec!["0.0.0.0:6379->6379/tcp".into()],
            created: 100,
        }];
        let map = extract_docker_ports(&containers);
        assert_eq!(map.get(&6379), Some(&"medusa_redis".to_string()));
        assert!(!is_port_available(6379, &map));
    }

    #[test]
    fn test_find_available_port_skips_docker_occupied() {
        let mut map = HashMap::new();
        map.insert(6379, "redis_old".into());

        let free = find_available_port(6379, &map);
        assert_ne!(free, 6379);
        assert!(free >= 6380);
    }

    #[test]
    fn test_find_free_port() {
        let port = find_free_port(15000);
        assert!(port >= 15000);
        assert!(is_port_free(port));
    }
}
