//! Find and remove unused containers, images and volumes (`asur clean`).

use std::io::IsTerminal;

use anyhow::Result;
use clap::Args;

use crate::docker::{ContainerRuntime, DockerRuntime};
use crate::ui;

#[derive(Args, Debug, Clone, Default)]
pub struct CleanArgs {
    /// Skip confirmation prompts and proceed with cleanup
    #[arg(short, long)]
    pub yes: bool,
    /// Include unused volumes for deletion (warning: irreversible data loss)
    #[arg(short, long)]
    pub volumes: bool,
    /// Preview reclaimable resources without deleting anything
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run(args: CleanArgs) -> Result<()> {
    let runtime = DockerRuntime::connect()?;
    execute(args, &runtime).await
}

pub async fn execute<R: ContainerRuntime>(args: CleanArgs, runtime: &R) -> Result<()> {
    let is_interactive = std::io::stdin().is_terminal();
    let inspection = ui::Spinner::new("Inspecting Docker resources...");
    let report = runtime.get_clean_report().await?;
    inspection.finish("Docker resource inspection complete.");

    let total_items = report.stopped_containers.len()
        + report.dangling_images.len()
        + report.unused_volumes.len();

    if total_items == 0 {
        ui::info(
            "No stopped containers, dangling images, or unused volumes to clean. (0 B to reclaim)",
        );
        return Ok(());
    }

    let containers_bytes: i64 = report.stopped_containers.iter().map(|(_, _, s)| *s).sum();
    let images_bytes: i64 = report.dangling_images.iter().map(|(_, _, s)| *s).sum();
    let volumes_bytes: i64 = report.unused_volumes.iter().map(|(_, s)| *s).sum();
    let total_reclaimable = containers_bytes + images_bytes + volumes_bytes;

    ui::section("Cleanable resources");
    let mut resource_rows = Vec::new();
    if !report.stopped_containers.is_empty() {
        resource_rows.push((
            "Stopped containers",
            format!(
                "{} (~{})",
                report.stopped_containers.len(),
                format_bytes(containers_bytes)
            ),
        ));
    }
    if !report.dangling_images.is_empty() {
        resource_rows.push((
            "Dangling images",
            format!(
                "{} (~{})",
                report.dangling_images.len(),
                format_bytes(images_bytes)
            ),
        ));
    }
    if !report.unused_volumes.is_empty() {
        resource_rows.push((
            "Unused volumes",
            format!(
                "{} (~{})",
                report.unused_volumes.len(),
                format_bytes(volumes_bytes)
            ),
        ));
    }
    resource_rows.push((
        "Total reclaimable",
        format!("~{}", format_bytes(total_reclaimable)),
    ));
    ui::key_values(&resource_rows);

    // 1. Dry run guard: preview only, never delete
    if args.dry_run {
        ui::info("[dry-run] Preview only. No resources were deleted.");
        return Ok(());
    }

    // 2. Non-interactive safety guard: require explicit --yes in scripts / CI
    if !is_interactive && !args.yes {
        ui::warn(
            "Non-interactive terminal detected: pass `--yes` to authorize deletion, or `--dry-run` to inspect.",
        );
        return Ok(());
    }

    // Build selectable options
    // Format: "[C] name (size)", "[I] tag (size)", "[V] name (size)"
    let mut options = Vec::new();
    // (type, Docker id/name, display name, size)
    let mut item_meta = Vec::new();

    for (id, name, size) in &report.stopped_containers {
        let label = format!("[Container] {:<24} ({})", name, format_bytes(*size));
        options.push(label);
        item_meta.push(('C', id.clone(), name.clone(), *size));
    }

    for (id, tag, size) in &report.dangling_images {
        let label = format!("[Image]     {:<24} ({})", tag, format_bytes(*size));
        options.push(label);
        item_meta.push(('I', id.clone(), tag.clone(), *size));
    }

    for (name, size) in &report.unused_volumes {
        let label = format!("[Volume]    {:<24} ({})", name, format_bytes(*size));
        options.push(label);
        item_meta.push(('V', name.clone(), name.clone(), *size));
    }

    let selected_indices = if is_interactive && !args.yes {
        let selection_formatter =
            |items: &[inquire::list_option::ListOption<&String>]| match items.len() {
                1 => "1 resource selected".to_string(),
                count => format!("{count} resources selected"),
            };
        let selected = inquire::MultiSelect::new("Select resources to remove:", options.clone())
            .with_help_message("Space: toggle  Enter: continue  Esc: cancel")
            .with_formatter(&selection_formatter)
            .prompt()
            .map_err(super::prompt_error)?;

        if selected.is_empty() {
            ui::info("No items selected.");
            return Ok(());
        }

        let mut indices = Vec::new();
        for s in selected {
            if let Some(pos) = options.iter().position(|opt| opt == &s) {
                indices.push(pos);
            }
        }
        indices
    } else {
        // Non-interactive with --yes, or interactive with --yes:
        // Always select containers and images.
        // VOLUMES ARE ONLY SELECTED IF EXPLICITLY REQUESTED WITH --volumes / -v!
        let mut indices = Vec::new();
        for (i, (kind, _, _, _)) in item_meta.iter().enumerate() {
            if *kind == 'V' {
                if args.volumes {
                    indices.push(i);
                }
            } else {
                indices.push(i);
            }
        }

        if !report.unused_volumes.is_empty() && !args.volumes {
            ui::info(
                "Volumes were excluded from automated cleanup. Pass `-v` or `--volumes` to include them.",
            );
        }

        indices
    };

    if selected_indices.is_empty() {
        ui::info("No matching items to clean.");
        return Ok(());
    }

    let mut container_ids = Vec::new();
    let mut image_ids = Vec::new();
    let mut volume_names = Vec::new();
    let mut total_reclaim_bytes: i64 = 0;

    if !args.yes && is_interactive {
        print_selected_resources(&selected_indices, &item_meta);
    }

    for idx in selected_indices {
        let (kind, id_or_name, _, size) = &item_meta[idx];
        total_reclaim_bytes += size;
        match kind {
            'C' => container_ids.push(id_or_name.clone()),
            'I' => image_ids.push(id_or_name.clone()),
            'V' => volume_names.push(id_or_name.clone()),
            _ => {}
        }
    }

    // Confirmation prompt in interactive mode
    if !args.yes && is_interactive {
        if !volume_names.is_empty() {
            ui::warn(
                "⚠️  Volume deletion is irreversible! All data inside selected volumes will be permanently lost.",
            );
        }

        let prompt_msg = format!(
            "Remove {} container(s), {} image(s), {} volume(s) (~{} space)?",
            container_ids.len(),
            image_ids.len(),
            volume_names.len(),
            format_bytes(total_reclaim_bytes)
        );

        let confirmed = inquire::Confirm::new(&prompt_msg)
            .with_default(false)
            .prompt()
            .map_err(super::prompt_error)?;

        if !confirmed {
            ui::info("Clean cancelled.");
            return Ok(());
        }
    }

    let cleaning = ui::Spinner::new("Removing selected Docker resources...");
    let results = runtime
        .clean_resources(&container_ids, &image_ids, &volume_names)
        .await?;
    cleaning.finish("Docker cleanup finished.");

    ui::success(&format!(
        "Removed {} container(s), {} image(s), {} volume(s).",
        results.containers_removed, results.images_removed, results.volumes_removed
    ));
    // Docker's delete endpoints do not report reclaimed bytes reliably. Never
    // claim the selected estimate as an actual reclaim total when any item
    // failed.
    if results.failures.is_empty() && total_reclaim_bytes > 0 {
        ui::hint(&format!(
            "Estimated reclaimed space: ~{}.",
            format_bytes(total_reclaim_bytes)
        ));
    }
    if !results.failures.is_empty() {
        ui::warn(&format!(
            "{} resource(s) could not be removed:",
            results.failures.len()
        ));
        for failure in &results.failures {
            eprintln!("  • {failure}");
        }
    }
    println!();

    Ok(())
}

fn print_selected_resources(selected: &[usize], items: &[(char, String, String, i64)]) {
    ui::section("Selected for removal");
    for kind in ['C', 'I', 'V'] {
        let heading = match kind {
            'C' => "Containers",
            'I' => "Images",
            'V' => "Volumes",
            _ => continue,
        };
        let matches: Vec<_> = selected
            .iter()
            .filter_map(|index| items.get(*index))
            .filter(|(item_kind, _, _, _)| *item_kind == kind)
            .collect();
        if matches.is_empty() {
            continue;
        }
        println!("  {heading} ({})", matches.len());
        for (_, _, name, size) in matches {
            println!("    - {name}  ({})", format_bytes(*size));
        }
    }
}

/// Convert bytes into human-readable string (KB, MB, GB).
pub fn format_bytes(bytes: i64) -> String {
    if bytes <= 0 {
        return "0 B".to_string();
    }

    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(10 * 1024 * 1024), "10.0 MB");
        assert_eq!(format_bytes(2 * 1024 * 1024 * 1024), "2.0 GB");
    }
}
