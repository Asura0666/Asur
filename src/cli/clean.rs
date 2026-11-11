//! Find and remove unused containers, images and volumes (`asur clean`).

use std::io::IsTerminal;

use anyhow::Result;
use clap::Args;
use owo_colors::{OwoColorize, Stream, Style};

use crate::docker::{ContainerRuntime, DockerRuntime};
use crate::ui;

#[derive(Args, Debug, Clone, Default)]
pub struct CleanArgs {
    /// Skip the confirmation prompt
    #[arg(short, long)]
    pub yes: bool,
}

pub async fn run(args: CleanArgs) -> Result<()> {
    let runtime = DockerRuntime::connect()?;
    execute(args, &runtime).await
}

pub async fn execute<R: ContainerRuntime>(args: CleanArgs, runtime: &R) -> Result<()> {
    let is_interactive = std::io::stdin().is_terminal();
    let report = runtime.get_clean_report().await?;

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

    println!();
    ui::info("Scanned cleanable resources:");
    if !report.stopped_containers.is_empty() {
        println!(
            "  • Stopped containers : {} (~{})",
            report.stopped_containers.len(),
            format_bytes(containers_bytes)
        );
    }
    if !report.dangling_images.is_empty() {
        println!(
            "  • Dangling images    : {} (~{})",
            report.dangling_images.len(),
            format_bytes(images_bytes)
        );
    }
    if !report.unused_volumes.is_empty() {
        println!(
            "  • Unused volumes     : {} (~{})",
            report.unused_volumes.len(),
            format_bytes(volumes_bytes)
        );
    }

    let cyan_bold = Style::new().cyan().bold();
    let space_styled = format!("~{}", format_bytes(total_reclaimable));
    println!(
        "  {} {}\n",
        "Total reclaimable space:".if_supports_color(Stream::Stdout, |t| t.bold()),
        space_styled.if_supports_color(Stream::Stdout, |t| t.style(cyan_bold))
    );

    // Build selectable options
    // Format: "[C] name (size)", "[I] tag (size)", "[V] name (size)"
    let mut options = Vec::new();
    let mut item_meta = Vec::new(); // (type: 'C'/'I'/'V', id_or_name: String, size: i64)

    for (id, name, size) in &report.stopped_containers {
        let label = format!("[Container] {:<24} ({})", name, format_bytes(*size));
        options.push(label);
        item_meta.push(('C', id.clone(), *size));
    }

    for (id, tag, size) in &report.dangling_images {
        let label = format!("[Image]     {:<24} ({})", tag, format_bytes(*size));
        options.push(label);
        item_meta.push(('I', id.clone(), *size));
    }

    for (name, size) in &report.unused_volumes {
        let label = format!("[Volume]    {:<24} ({})", name, format_bytes(*size));
        options.push(label);
        item_meta.push(('V', name.clone(), *size));
    }

    let selected_indices = if is_interactive && !args.yes {
        let selected = inquire::MultiSelect::new(
            "Select resources to remove (Space to toggle, Enter to confirm):",
            options.clone(),
        )
        .prompt()
        .map_err(|e| anyhow::anyhow!("selection cancelled: {e}"))?;

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
        // Non-interactive or --yes: select all
        (0..options.len()).collect()
    };

    let mut container_ids = Vec::new();
    let mut image_ids = Vec::new();
    let mut volume_names = Vec::new();
    let mut total_reclaim_bytes: i64 = 0;

    for idx in selected_indices {
        let (kind, id_or_name, size) = &item_meta[idx];
        total_reclaim_bytes += size;
        match kind {
            'C' => container_ids.push(id_or_name.clone()),
            'I' => image_ids.push(id_or_name.clone()),
            'V' => volume_names.push(id_or_name.clone()),
            _ => {}
        }
    }

    // Confirmation prompt
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
            .unwrap_or(false);

        if !confirmed {
            ui::info("Clean cancelled.");
            return Ok(());
        }
    }

    ui::info("Cleaning selected resources...");
    let mut results = runtime
        .clean_resources(&container_ids, &image_ids, &volume_names)
        .await?;
    results.space_reclaimed_bytes = total_reclaim_bytes;

    let green_bold = Style::new().green().bold();
    let check = "✔".if_supports_color(Stream::Stdout, |t| t.style(green_bold));

    println!(
        "\n{check} Removed {} container(s), {} image(s), {} volume(s).",
        results.containers_removed, results.images_removed, results.volumes_removed
    );
    if results.space_reclaimed_bytes > 0 {
        println!(
            "  Reclaimed ~{} of disk space.",
            format_bytes(results.space_reclaimed_bytes)
        );
    }
    println!();

    Ok(())
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
