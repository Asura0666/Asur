//! Validated, runtime-independent container and image configuration types.

use std::fmt;
use std::str::FromStr;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PortProtocol {
    Tcp,
    Udp,
}

impl fmt::Display for PortProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        })
    }
}

impl FromStr for PortProtocol {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "tcp" => Ok(Self::Tcp),
            "udp" => Ok(Self::Udp),
            _ => bail!("unsupported port protocol `{value}`; expected `tcp` or `udp`"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortBinding {
    pub host_ip: Option<String>,
    pub host_port: u16,
    pub container_port: u16,
    pub protocol: PortProtocol,
}

impl PortBinding {
    pub fn tcp(host_port: u16, container_port: u16) -> Self {
        Self {
            host_ip: None,
            host_port,
            container_port,
            protocol: PortProtocol::Tcp,
        }
    }

    pub fn container_key(&self) -> String {
        format!("{}/{}", self.container_port, self.protocol)
    }
}

impl fmt::Display for PortBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(ip) = &self.host_ip {
            write!(
                f,
                "{ip}:{}:{}/{}",
                self.host_port, self.container_port, self.protocol
            )
        } else {
            write!(
                f,
                "{}:{}/{}",
                self.host_port, self.container_port, self.protocol
            )
        }
    }
}

impl FromStr for PortBinding {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let value = value.trim();
        let (mapping, protocol) = match value.rsplit_once('/') {
            Some((mapping, protocol)) => (mapping, protocol.parse()?),
            None => (value, PortProtocol::Tcp),
        };

        let mut parts = mapping.rsplitn(3, ':');
        let container = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("invalid port mapping `{value}`"))?;
        let host = parts.next().ok_or_else(|| {
            anyhow::anyhow!(
                "invalid port mapping `{value}`; expected HOST_PORT:CONTAINER_PORT[/protocol]"
            )
        })?;
        let host_ip = parts.next().map(|ip| {
            ip.trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_string()
        });

        let host_port = parse_port(host, "host", value)?;
        let container_port = parse_port(container, "container", value)?;
        if host_ip.as_deref() == Some("") {
            bail!("host address cannot be empty in port mapping `{value}`");
        }

        Ok(Self {
            host_ip,
            host_port,
            container_port,
            protocol,
        })
    }
}

fn parse_port(value: &str, kind: &str, mapping: &str) -> Result<u16> {
    let port = value
        .parse::<u16>()
        .map_err(|_| anyhow::anyhow!("invalid {kind} port in mapping `{mapping}`"))?;
    if port == 0 {
        bail!("{kind} port must be between 1 and 65535 in mapping `{mapping}`");
    }
    Ok(port)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MountKind {
    Bind,
    Volume,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mount {
    pub kind: MountKind,
    pub source: String,
    pub target: String,
    pub read_only: bool,
}

impl Mount {
    pub fn volume(source: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            kind: MountKind::Volume,
            source: source.into(),
            target: target.into(),
            read_only: false,
        }
    }
}

impl fmt::Display for Mount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.source, self.target)?;
        if self.read_only {
            f.write_str(":ro")?;
        }
        Ok(())
    }
}

impl FromStr for Mount {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let value = value.trim();
        if value.contains('=') {
            return parse_long_mount(value);
        }

        let (without_mode, read_only) = match value.rsplit_once(':') {
            Some((rest, "ro" | "readonly")) => (rest, true),
            _ => (value, false),
        };
        let (source, target) = without_mode.rsplit_once(':').ok_or_else(|| {
            anyhow::anyhow!(
                "invalid mount `{value}`; expected SOURCE:TARGET[:ro] or Docker --mount syntax"
            )
        })?;
        validate_mount_parts(source, target, value)?;
        Ok(Self {
            kind: infer_mount_kind(source),
            source: source.to_string(),
            target: target.to_string(),
            read_only,
        })
    }
}

fn parse_long_mount(value: &str) -> Result<Mount> {
    let mut kind = None;
    let mut source = None;
    let mut target = None;
    let mut read_only = false;

    for item in value.split(',') {
        let item = item.trim();
        let (key, val) = item.split_once('=').unwrap_or((item, ""));
        match key {
            "type" => {
                kind = Some(match val {
                    "bind" => MountKind::Bind,
                    "volume" => MountKind::Volume,
                    _ => bail!("unsupported mount type `{val}`; expected `bind` or `volume`"),
                });
            }
            "source" | "src" => source = Some(val),
            "target" | "destination" | "dst" => target = Some(val),
            "readonly" | "read-only" | "ro" if val.is_empty() || val == "true" => {
                read_only = true;
            }
            "readonly" | "read-only" | "ro" if val == "false" => {}
            _ => bail!("unsupported mount option `{item}` in `{value}`"),
        }
    }

    let source = source.ok_or_else(|| anyhow::anyhow!("mount `{value}` is missing `source`"))?;
    let target = target.ok_or_else(|| anyhow::anyhow!("mount `{value}` is missing `target`"))?;
    validate_mount_parts(source, target, value)?;
    Ok(Mount {
        kind: kind.unwrap_or_else(|| infer_mount_kind(source)),
        source: source.to_string(),
        target: target.to_string(),
        read_only,
    })
}

fn infer_mount_kind(source: &str) -> MountKind {
    if source.starts_with('.')
        || source.starts_with('/')
        || source.starts_with('~')
        || source.contains('\\')
        || source.as_bytes().get(1) == Some(&b':')
    {
        MountKind::Bind
    } else {
        MountKind::Volume
    }
}

fn validate_mount_parts(source: &str, target: &str, original: &str) -> Result<()> {
    if source.trim().is_empty() || target.trim().is_empty() {
        bail!("mount source and target cannot be empty in `{original}`");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    No,
    Always,
    #[default]
    UnlessStopped,
    OnFailure {
        maximum_retry_count: u64,
    },
}

impl fmt::Display for RestartPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::No => f.write_str("no"),
            Self::Always => f.write_str("always"),
            Self::UnlessStopped => f.write_str("unless-stopped"),
            Self::OnFailure {
                maximum_retry_count: 0,
            } => f.write_str("on-failure"),
            Self::OnFailure {
                maximum_retry_count,
            } => write!(f, "on-failure:{maximum_retry_count}"),
        }
    }
}

impl FromStr for RestartPolicy {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "no" | "none" => Ok(Self::No),
            "always" => Ok(Self::Always),
            "unless-stopped" => Ok(Self::UnlessStopped),
            "on-failure" => Ok(Self::OnFailure {
                maximum_retry_count: 0,
            }),
            value if value.starts_with("on-failure:") => {
                let retries = value["on-failure:".len()..].parse::<u64>().map_err(|_| {
                    anyhow::anyhow!(
                        "invalid restart policy `{value}`; retry count must be a number"
                    )
                })?;
                Ok(Self::OnFailure {
                    maximum_retry_count: retries,
                })
            }
            _ => bail!(
                "invalid restart policy `{value}`; expected no, on-failure[:N], always, or unless-stopped"
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PullPolicy {
    #[default]
    Missing,
    Always,
    Never,
}

impl fmt::Display for PullPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Missing => "missing",
            Self::Always => "always",
            Self::Never => "never",
        })
    }
}

impl FromStr for PullPolicy {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "missing" => Ok(Self::Missing),
            "always" => Ok(Self::Always),
            "never" => Ok(Self::Never),
            _ => bail!("invalid pull policy `{value}`; expected missing, always, or never"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageReference {
    original: String,
    repository: String,
    tag: Option<String>,
    digest: Option<String>,
}

impl ImageReference {
    pub fn as_str(&self) -> &str {
        &self.original
    }

    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// Docker's pull API interprets an empty tag as "all tags". Always return
    /// one explicit tag or digest so an implicit reference pulls only latest.
    pub fn pull_tag(&self) -> &str {
        self.digest
            .as_deref()
            .or(self.tag.as_deref())
            .unwrap_or("latest")
    }

    pub fn registry(&self) -> &str {
        let first = self.repository.split('/').next().unwrap_or_default();
        match first {
            "index.docker.io" | "registry-1.docker.io" => "docker.io",
            value if value == "localhost" || value.contains('.') || value.contains(':') => value,
            _ => "docker.io",
        }
    }

    pub fn is_qualified_target(value: &str) -> bool {
        let last = value.rsplit('/').next().unwrap_or(value);
        value.contains('/') || value.contains('@') || last.contains(':')
    }

    pub fn uses_latest(&self) -> bool {
        self.digest.is_none() && self.tag.as_deref().unwrap_or("latest") == "latest"
    }

    pub fn suggested_container_name(&self) -> String {
        let leaf = self.repository.rsplit('/').next().unwrap_or("image");
        let cleaned: String = leaf
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        format!("asur-{cleaned}")
    }

    pub fn pinned_with(&self, digest: &str) -> String {
        format!("{}@{digest}", self.repository)
    }
}

impl fmt::Display for ImageReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.original)
    }
}

impl FromStr for ImageReference {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let original = value.trim();
        if original.is_empty() || original.chars().any(char::is_whitespace) {
            bail!("image reference cannot be empty or contain whitespace");
        }

        let mut at_parts = original.split('@');
        let name_and_tag = at_parts.next().unwrap_or_default();
        let digest = at_parts.next().map(str::to_string);
        if at_parts.next().is_some() {
            bail!("invalid image reference `{original}`: more than one `@`");
        }
        if let Some(digest) = &digest {
            validate_digest(digest, original)?;
        }

        let last_slash = name_and_tag.rfind('/');
        let last_colon = name_and_tag.rfind(':');
        let (repository, tag) = match last_colon
            .filter(|colon| last_slash.map(|slash| *colon > slash).unwrap_or(true))
        {
            Some(colon) => (
                &name_and_tag[..colon],
                Some(name_and_tag[colon + 1..].to_string()),
            ),
            None => (name_and_tag, None),
        };

        validate_repository(repository, original)?;
        if let Some(tag) = &tag {
            validate_tag(tag, original)?;
        }

        Ok(Self {
            original: original.to_string(),
            repository: repository.to_string(),
            tag,
            digest,
        })
    }
}

fn validate_repository(repository: &str, original: &str) -> Result<()> {
    if repository.is_empty()
        || repository.starts_with('/')
        || repository.ends_with('/')
        || repository.contains("//")
    {
        bail!("invalid image repository in `{original}`");
    }
    if repository.chars().any(|c| c.is_ascii_uppercase()) {
        bail!("invalid image reference `{original}`: repository names must be lowercase");
    }
    let valid = repository.chars().all(|c| {
        c.is_ascii_lowercase()
            || c.is_ascii_digit()
            || matches!(c, '.' | '_' | '-' | '/' | ':')
            || c == '['
            || c == ']'
    });
    if !valid {
        bail!("invalid character in image repository `{repository}`");
    }
    Ok(())
}

fn validate_tag(tag: &str, original: &str) -> Result<()> {
    let valid = !tag.is_empty()
        && tag.len() <= 128
        && tag
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if !valid {
        bail!("invalid image tag in `{original}`");
    }
    Ok(())
}

fn validate_digest(digest: &str, original: &str) -> Result<()> {
    let (algorithm, encoded) = digest.split_once(':').ok_or_else(|| {
        anyhow::anyhow!("invalid digest in `{original}`; expected ALGORITHM:VALUE")
    })?;
    if algorithm.is_empty()
        || !algorithm
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '.' | '-'))
        || encoded.is_empty()
        || !encoded
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '=' | '-'))
    {
        bail!("invalid digest in image reference `{original}`");
    }
    if algorithm == "sha256"
        && (encoded.len() != 64 || !encoded.chars().all(|c| c.is_ascii_hexdigit()))
    {
        bail!("invalid sha256 digest in `{original}`; expected 64 hexadecimal characters");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tcp_udp_and_explicit_ip_ports() {
        assert_eq!(
            "8080:80".parse::<PortBinding>().unwrap(),
            PortBinding::tcp(8080, 80)
        );
        let udp = "127.0.0.1:5353:53/udp".parse::<PortBinding>().unwrap();
        assert_eq!(udp.host_ip.as_deref(), Some("127.0.0.1"));
        assert_eq!(udp.protocol, PortProtocol::Udp);
    }

    #[test]
    fn rejects_invalid_ports() {
        assert!("80".parse::<PortBinding>().is_err());
        assert!("0:80".parse::<PortBinding>().is_err());
        assert!("8080:70000".parse::<PortBinding>().is_err());
    }

    #[test]
    fn parses_short_and_long_mounts() {
        let short = "cache:/data:ro".parse::<Mount>().unwrap();
        assert_eq!(short.kind, MountKind::Volume);
        assert!(short.read_only);

        let long = "type=bind,source=./src,target=/app,readonly"
            .parse::<Mount>()
            .unwrap();
        assert_eq!(long.kind, MountKind::Bind);
        assert_eq!(long.target, "/app");
        assert!(long.read_only);
    }

    #[test]
    fn parses_windows_mount_source() {
        let mount = r"C:\work\data:/data:ro".parse::<Mount>().unwrap();
        assert_eq!(mount.source, r"C:\work\data");
        assert_eq!(mount.target, "/data");
        assert!(mount.read_only);
    }

    #[test]
    fn parses_image_tags_digests_and_registries() {
        let tagged = "ghcr.io/acme/orders:1.4".parse::<ImageReference>().unwrap();
        assert_eq!(tagged.registry(), "ghcr.io");
        assert_eq!(tagged.repository(), "ghcr.io/acme/orders");
        assert_eq!(tagged.tag.as_deref(), Some("1.4"));
        assert_eq!(tagged.pull_tag(), "1.4");

        let digest = format!("redis@sha256:{}", "a".repeat(64))
            .parse::<ImageReference>()
            .unwrap();
        assert_eq!(digest.registry(), "docker.io");
        assert!(digest.digest.is_some());
        assert!(digest.pull_tag().starts_with("sha256:"));
        assert_eq!(
            "nginx".parse::<ImageReference>().unwrap().pull_tag(),
            "latest"
        );
    }

    #[test]
    fn canonicalizes_docker_hub_registry_aliases() {
        let index = "index.docker.io/typesense/typesense:30.0"
            .parse::<ImageReference>()
            .unwrap();
        let registry = "registry-1.docker.io/library/redis:7"
            .parse::<ImageReference>()
            .unwrap();
        assert_eq!(index.registry(), "docker.io");
        assert_eq!(registry.registry(), "docker.io");
    }

    #[test]
    fn classifies_explicit_generic_targets() {
        assert!(ImageReference::is_qualified_target("redis:7-alpine"));
        assert!(ImageReference::is_qualified_target("ghcr.io/acme/api"));
        assert!(ImageReference::is_qualified_target("repo@sha256:abc"));
        assert!(!ImageReference::is_qualified_target("nginx"));
    }

    #[test]
    fn rejects_bad_references() {
        assert!("Repo/Image:tag".parse::<ImageReference>().is_err());
        assert!("nginx:".parse::<ImageReference>().is_err());
        assert!("nginx@sha256:abc".parse::<ImageReference>().is_err());
    }

    #[test]
    fn parses_restart_and_pull_policies() {
        assert_eq!(
            "on-failure:4".parse::<RestartPolicy>().unwrap(),
            RestartPolicy::OnFailure {
                maximum_retry_count: 4
            }
        );
        assert_eq!(
            "unless-stopped".parse::<RestartPolicy>().unwrap(),
            RestartPolicy::UnlessStopped
        );
        assert_eq!("never".parse::<PullPolicy>().unwrap(), PullPolicy::Never);
        assert!("sometimes".parse::<PullPolicy>().is_err());
    }
}
