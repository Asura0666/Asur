//! Local image discovery and Docker Hub search commands.

use std::io::IsTerminal;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use crate::docker::{
    ContainerRuntime, DockerRuntime, ImageMetadata, ImageReference, ImageSearchResult, ImageTag,
    LocalImage,
};
use crate::ui;

#[derive(Args, Debug, Clone)]
#[command(
    long_about = "Discover Docker Hub repositories and tags, list images already downloaded by Docker, or inspect a local image's ports, volumes, digest, entrypoint, and command. Search and tags do not download anything."
)]
pub struct ImageArgs {
    #[command(subcommand)]
    pub command: ImageCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ImageCommand {
    /// Search Docker Hub (registry search is provider-specific)
    Search {
        /// Repository name or keyword
        query: String,
        /// Maximum number of results
        #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u16).range(1..=50))]
        limit: u16,
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Show the tags that can actually be pulled from a Docker Hub repository
    #[command(alias = "tag")]
    Tags {
        /// Docker Hub repository or search name, such as typesense/typesense or typesense
        repository: String,
        /// Maximum number of tags to display
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u16).range(1..=100))]
        limit: u16,
        /// Include release candidates, rolling aliases, and CPU-specific builds
        #[arg(long)]
        all: bool,
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// List images in the local Docker daemon
    Ls {
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Inspect a local image's OCI/runtime metadata
    Inspect {
        /// Image tag, digest, or local image ID
        reference: String,
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
}

pub async fn run(args: ImageArgs) -> Result<()> {
    let runtime = DockerRuntime::connect()?;
    execute(args, &runtime).await
}

pub async fn execute<R: ContainerRuntime>(args: ImageArgs, runtime: &R) -> Result<()> {
    match args.command {
        ImageCommand::Search { query, limit, json } => {
            let results = runtime.search_images(&query, usize::from(limit)).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&results)?);
                return Ok(());
            }
            if results.is_empty() {
                ui::warn(&format!("Docker Hub returned no results for `{query}`."));
                return Ok(());
            }
            println!("Docker Hub repositories for `{query}`");
            for (index, result) in results.into_iter().enumerate() {
                let status = result_status(&result, &query);
                let description = truncate(result.description.trim(), 64);
                let suffix = if description.is_empty() {
                    String::new()
                } else {
                    format!(" — {description}")
                };
                println!(
                    "  {:>2}. {}{}{}",
                    index + 1,
                    result.name,
                    status.map_or_else(String::new, |value| format!("  [{value}]")),
                    suffix,
                );
            }
            ui::hint(
                "[official] is Docker Hub's badge; [exact match] is only a name match. Check versions with `asur image tags <repository>`.",
            );
        }
        ImageCommand::Tags {
            repository,
            limit,
            all,
            json,
        } => {
            let repository = resolve_docker_hub_repository(runtime, &repository, json).await?;
            let fetch_limit = if all {
                usize::from(limit)
            } else {
                usize::from(limit).max(200)
            };
            let discovered = runtime.list_image_tags(&repository, fetch_limit).await?;
            let mut tags = if all {
                discovered
            } else {
                stable_tags(&discovered)
            };
            tags.truncate(usize::from(limit));
            if json {
                println!("{}", serde_json::to_string_pretty(&tags)?);
                return Ok(());
            }
            if tags.is_empty() {
                if all {
                    ui::warn(&format!("Docker Hub returned no tags for `{repository}`."));
                } else {
                    ui::warn(&format!(
                        "Docker Hub returned no stable version tags for `{repository}`. Use `--all` to include rolling and pre-release tags."
                    ));
                }
                return Ok(());
            }
            println!(
                "{} tags for `{repository}`",
                if all { "Recent" } else { "Stable" }
            );
            println!("  {:<24} {:>10}  {:<14} PLATFORM", "TAG", "SIZE", "RELEASE");
            for tag in tags {
                let size = tag
                    .size
                    .map(super::clean::format_bytes)
                    .unwrap_or_else(dash);
                let platforms = if tag.platforms.is_empty() {
                    dash()
                } else {
                    truncate(&tag.platforms.join(", "), 48)
                };
                println!(
                    "  {:<24} {:>10}  {:<14} {platforms}",
                    tag.name,
                    size,
                    tag_channel(&tag.name)
                );
            }
            ui::hint(
                "Docker Hub does not publish LTS status. `stable` means the tag has no common pre-release or CPU-specific marker; confirm support policy in the publisher's release notes.",
            );
            if !all {
                ui::hint("Use `--all` to include release candidates and CPU-specific builds.");
            }
            ui::hint(&format!("Run one with: asur up --image {repository}:<tag>"));
        }
        ImageCommand::Ls { json } => {
            let images = runtime.list_images().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&images)?);
                return Ok(());
            }
            if images.is_empty() {
                ui::info("No local images found.");
                return Ok(());
            }
            println!("{:<48} {:<20} {:>10}", "REFERENCE", "IMAGE ID", "SIZE");
            for image in images {
                let references = if image.tags.is_empty() {
                    vec!["<none>:<none>".to_string()]
                } else {
                    image.tags
                };
                let id = image.id.strip_prefix("sha256:").unwrap_or(&image.id);
                for (index, reference) in references.into_iter().enumerate() {
                    println!(
                        "{:<48} {:<20} {:>10}",
                        truncate(&reference, 47),
                        if index == 0 {
                            &id[..id.len().min(12)]
                        } else {
                            ""
                        },
                        if index == 0 {
                            super::clean::format_bytes(image.size)
                        } else {
                            String::new()
                        }
                    );
                }
            }
        }
        ImageCommand::Inspect { reference, json } => {
            let metadata = match runtime.inspect_image(&reference).await {
                Ok(metadata) => metadata,
                Err(exact_error) => {
                    let candidates = if ImageReference::is_qualified_target(&reference) {
                        Vec::new()
                    } else {
                        local_reference_candidates(&runtime.list_images().await?, &reference)
                    };
                    if candidates.is_empty() {
                        if std::io::stdin().is_terminal() && !json {
                            ui::warn(&format!(
                                "No downloaded image matches `{reference}`. Use `asur image ls` to see local images, or pull one with `asur up --image <repository>:<tag>`."
                            ));
                            return Ok(());
                        }
                        return Err(exact_error).with_context(|| {
                            format!(
                                "could not inspect local image `{reference}`. Pull or build it first"
                            )
                        });
                    }

                    let resolved = if candidates.len() == 1 {
                        candidates[0].clone()
                    } else if std::io::stdin().is_terminal() && !json {
                        inquire::Select::new(
                            &format!("Multiple local images match `{reference}`. Choose one:"),
                            candidates,
                        )
                        .with_help_message("Esc: cancel and exit")
                        .prompt()
                        .map_err(super::prompt_error)?
                    } else {
                        bail!(
                            "multiple local images match `{reference}`; inspect one exact reference from `asur image ls`"
                        );
                    };
                    if !json {
                        ui::info(&format!("Using downloaded image `{resolved}`."));
                    }
                    runtime.inspect_image(&resolved).await.with_context(|| {
                        format!("could not inspect resolved local image `{resolved}`")
                    })?
                }
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&metadata)?);
            } else {
                print_metadata(&metadata);
            }
        }
    }
    Ok(())
}

async fn resolve_docker_hub_repository<R: ContainerRuntime>(
    runtime: &R,
    value: &str,
    json: bool,
) -> Result<String> {
    if ImageReference::is_qualified_target(value) {
        return Ok(value.to_string());
    }
    let results = runtime.search_images(value, 12).await?;
    if results.is_empty() {
        bail!("Docker Hub returned no repositories for `{value}`");
    }
    if let Some(result) = best_repository_match(&results, value) {
        if !json && result.name != value {
            ui::info(&format!("Resolved `{value}` to `{}`.", result.name));
        }
        return Ok(result.name.clone());
    }
    if std::io::stdin().is_terminal() && !json {
        let names: Vec<String> = results.into_iter().map(|result| result.name).collect();
        return inquire::Select::new(
            &format!("Choose the Docker Hub repository for `{value}`:"),
            names,
        )
        .with_help_message("Esc: cancel and exit")
        .prompt()
        .map_err(super::prompt_error);
    }
    bail!(
        "`{value}` is not an exact Docker Hub repository; use a full name from `asur image search {value}`"
    )
}

fn best_repository_match<'a>(
    results: &'a [ImageSearchResult],
    query: &str,
) -> Option<&'a ImageSearchResult> {
    let query = query.trim().to_ascii_lowercase();
    results.iter().find(|result| {
        let name = result.name.to_ascii_lowercase();
        name == query || name == format!("{query}/{query}")
    })
}

fn stable_tags(tags: &[ImageTag]) -> Vec<ImageTag> {
    let mut tags: Vec<ImageTag> = tags
        .iter()
        .filter(|tag| tag_channel(&tag.name) == "stable")
        .cloned()
        .collect();
    tags.sort_by(|left, right| {
        match (
            numeric_tag_version(&left.name),
            numeric_tag_version(&right.name),
        ) {
            (Some(left), Some(right)) => right.cmp(&left),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => left.name.cmp(&right.name),
        }
    });
    tags.dedup_by(|left, right| left.name == right.name);
    tags
}

fn numeric_tag_version(tag: &str) -> Option<Vec<u64>> {
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

fn local_reference_candidates(images: &[LocalImage], query: &str) -> Vec<String> {
    let Ok(query) = query.parse::<ImageReference>() else {
        return Vec::new();
    };
    let query_repository = query.repository();
    let mut candidates: Vec<String> = images
        .iter()
        .flat_map(|image| image.tags.iter())
        .filter_map(|tag| {
            let reference = tag.parse::<ImageReference>().ok()?;
            let repository = reference.repository();
            let leaf = repository.rsplit('/').next().unwrap_or(repository);
            (repository == query_repository || leaf == query_repository).then(|| tag.clone())
        })
        .collect();
    candidates.sort();
    candidates.dedup();
    candidates
}

fn print_metadata(metadata: &ImageMetadata) {
    ui::key_values(&[
        ("Reference", metadata.reference.clone()),
        ("ID", display_or_dash(&metadata.id)),
        (
            "Digest",
            metadata.digests.first().cloned().unwrap_or_else(dash),
        ),
        ("Platform", platform(metadata)),
        ("Size", super::clean::format_bytes(metadata.size)),
        ("Exposed ports", joined_or_dash(&metadata.exposed_ports)),
        ("Volumes", joined_or_dash(&metadata.declared_volumes)),
        ("Entrypoint", joined_or_dash(&metadata.entrypoint)),
        ("Command", joined_or_dash(&metadata.command)),
    ]);
    if !metadata.labels.is_empty() {
        println!("  Labels:");
        for (key, value) in &metadata.labels {
            println!("    {key}={value}");
        }
    }
}

fn result_status(result: &crate::docker::ImageSearchResult, query: &str) -> Option<&'static str> {
    if result.official == Some(true) {
        return Some("official");
    }
    if result.verified == Some(true) {
        return Some("verified publisher");
    }
    let query = query.trim().to_ascii_lowercase();
    let name = result.name.to_ascii_lowercase();
    let exact_namespace_match = name == query || name == format!("{query}/{query}");
    exact_namespace_match.then_some("exact match")
}

fn platform(metadata: &ImageMetadata) -> String {
    match (metadata.os.as_str(), metadata.architecture.as_str()) {
        ("", "") => dash(),
        (os, "") => os.to_string(),
        ("", arch) => arch.to_string(),
        (os, arch) => format!("{os}/{arch}"),
    }
}

fn joined_or_dash(values: &[String]) -> String {
    if values.is_empty() {
        dash()
    } else {
        values.join(" ")
    }
}

fn display_or_dash(value: &str) -> String {
    if value.is_empty() {
        dash()
    } else {
        value.to_string()
    }
}

fn dash() -> String {
    "—".to_string()
}

fn tag_channel(tag: &str) -> &'static str {
    let tag = tag.to_ascii_lowercase();
    if [
        "amd64", "arm64", "armv7", "armv8", "x86_64", "ppc64", "s390x",
    ]
    .iter()
    .any(|platform| tag.contains(platform))
    {
        "CPU-specific"
    } else if [
        "rc", "alpha", "beta", "dev", "nightly", "snapshot", "canary", "edge",
    ]
    .iter()
    .any(|marker| tag.contains(marker))
    {
        "pre-release"
    } else if tag == "latest" {
        "rolling"
    } else {
        "stable"
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let mut chars = value.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        let mut shortened: String = prefix.chars().take(max_chars.saturating_sub(1)).collect();
        shortened.push('…');
        shortened
    } else {
        prefix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_is_unicode_safe() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("工具箱", 2), "工…");
        assert_eq!(truncate("short", 10), "short");
    }

    #[test]
    fn platform_handles_missing_fields() {
        let metadata = ImageMetadata {
            os: "linux".into(),
            architecture: "amd64".into(),
            ..Default::default()
        };
        assert_eq!(platform(&metadata), "linux/amd64");
    }

    #[test]
    fn search_status_does_not_call_a_name_match_official() {
        let result = crate::docker::ImageSearchResult {
            name: "typesense/typesense".into(),
            ..Default::default()
        };
        assert_eq!(result_status(&result, "typesense"), Some("exact match"));

        let official = crate::docker::ImageSearchResult {
            name: "redis".into(),
            official: Some(true),
            ..Default::default()
        };
        assert_eq!(result_status(&official, "redis"), Some("official"));
    }

    #[test]
    fn bare_inspect_reference_finds_downloaded_tags() {
        let images = vec![LocalImage {
            tags: vec![
                "redis:7-alpine".into(),
                "docker.io/typesense/typesense:30.2".into(),
            ],
            ..Default::default()
        }];
        assert_eq!(
            local_reference_candidates(&images, "redis"),
            vec!["redis:7-alpine"]
        );
        assert_eq!(
            local_reference_candidates(&images, "typesense"),
            vec!["docker.io/typesense/typesense:30.2"]
        );
    }

    #[test]
    fn tag_channel_does_not_claim_lts_status() {
        assert_eq!(tag_channel("30.2"), "stable");
        assert_eq!(tag_channel("31.0.rc25"), "pre-release");
        assert_eq!(tag_channel("31.0.rc25-amd64"), "CPU-specific");
        assert_eq!(tag_channel("latest"), "rolling");
    }

    #[test]
    fn stable_tag_list_hides_variants_and_sorts_versions() {
        let tags = vec![
            ImageTag {
                name: "31.0.rc25".into(),
                ..Default::default()
            },
            ImageTag {
                name: "30.0".into(),
                ..Default::default()
            },
            ImageTag {
                name: "30.2".into(),
                ..Default::default()
            },
            ImageTag {
                name: "30.2-amd64".into(),
                ..Default::default()
            },
            ImageTag {
                name: "29.1".into(),
                ..Default::default()
            },
        ];
        assert_eq!(
            stable_tags(&tags)
                .into_iter()
                .map(|tag| tag.name)
                .collect::<Vec<_>>(),
            vec!["30.2", "30.0", "29.1"]
        );
    }

    #[test]
    fn bare_repository_prefers_matching_namespace_name() {
        let results = vec![
            ImageSearchResult {
                name: "unrelated/typesense".into(),
                ..Default::default()
            },
            ImageSearchResult {
                name: "typesense/typesense".into(),
                ..Default::default()
            },
        ];
        assert_eq!(
            best_repository_match(&results, "typesense").map(|result| result.name.as_str()),
            Some("typesense/typesense")
        );
    }
}
