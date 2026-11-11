//! View and follow container logs (`asur logs`).

use std::io::IsTerminal;

use anyhow::Result;
use clap::Args;
use owo_colors::{OwoColorize, Stream, Style};
use tokio::sync::mpsc;

use crate::docker::{ContainerInfo, ContainerRuntime, DockerRuntime};
use crate::ui;

#[derive(Args, Debug, Clone, Default)]
pub struct LogsArgs {
    /// Container name or id (omit to pick from a list)
    pub container: Option<String>,
    /// Number of lines to show from the end
    #[arg(short = 't', long, default_value_t = 100)]
    pub tail: usize,
    /// Keep streaming new log lines
    #[arg(short, long)]
    pub follow: bool,
}

pub async fn run(args: LogsArgs) -> Result<()> {
    let runtime = DockerRuntime::connect()?;
    execute(args, &runtime).await
}

pub async fn execute<R: ContainerRuntime>(args: LogsArgs, runtime: &R) -> Result<()> {
    let is_interactive = std::io::stdin().is_terminal();
    let all_containers = runtime.list_containers(true).await?;

    if all_containers.is_empty() {
        ui::info("No containers found.");
        return Ok(());
    }

    let target = if let Some(c) = args.container {
        c
    } else if is_interactive {
        select_container_interactive(&all_containers)?
    } else {
        ui::info(
            "No container specified. Run `asur logs <container>` or run in an interactive terminal.",
        );
        return Ok(());
    };

    ui::info(&format!(
        "Showing logs for `{target}` (tail: {}, follow: {})...",
        args.tail, args.follow
    ));

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let mut stream_fut = std::pin::pin!(runtime.stream_logs(&target, args.tail, args.follow, tx));

    loop {
        tokio::select! {
            res = &mut stream_fut => {
                if let Err(e) = res {
                    ui::error(&format!("{e:#}"));
                }
                // Drain any remaining buffered lines
                while let Ok(line) = rx.try_recv() {
                    let formatted = colorize_log_line(&line);
                    print!("{formatted}");
                    if !formatted.ends_with('\n') {
                        println!();
                    }
                }
                break;
            }
            line = rx.recv() => {
                match line {
                    Some(l) => {
                        let formatted = colorize_log_line(&l);
                        print!("{formatted}");
                        if !formatted.ends_with('\n') {
                            println!();
                        }
                    }
                    None => break,
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!();
                ui::info("Log streaming stopped.");
                break;
            }
        }
    }

    Ok(())
}

/// Syntax highlight common log levels (ERROR, WARN, INFO, DEBUG) in a log line.
pub fn colorize_log_line(line: &str) -> String {
    let lower = line.to_lowercase();

    if lower.contains("error") || lower.contains("err") || lower.contains("fatal") {
        let style = Style::new().red().bold();
        line.if_supports_color(Stream::Stdout, |t| t.style(style))
            .to_string()
    } else if lower.contains("warn") || lower.contains("warning") {
        let style = Style::new().yellow();
        line.if_supports_color(Stream::Stdout, |t| t.style(style))
            .to_string()
    } else if lower.contains("debug") || lower.contains("trace") {
        let style = Style::new().dimmed();
        line.if_supports_color(Stream::Stdout, |t| t.style(style))
            .to_string()
    } else if lower.contains("ready") || lower.contains("started") || lower.contains("success") {
        let style = Style::new().green();
        line.if_supports_color(Stream::Stdout, |t| t.style(style))
            .to_string()
    } else {
        line.to_string()
    }
}

/// Searchable interactive selection of a single container for logs.
fn select_container_interactive(containers: &[ContainerInfo]) -> Result<String> {
    let options: Vec<String> = containers
        .iter()
        .map(|c| format!("{:<20} {:<24} ({})", c.name, c.image, c.status))
        .collect();

    let choice = inquire::Select::new("Select a container to view logs:", options)
        .prompt()
        .map_err(|e| anyhow::anyhow!("selection cancelled: {e}"))?;

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
    fn test_colorize_error() {
        let line = "[ERROR] database connection lost";
        let colored = colorize_log_line(line);
        assert!(colored.contains(line));
    }

    #[test]
    fn test_colorize_warn() {
        let line = "[WARN] memory usage above 80%";
        let colored = colorize_log_line(line);
        assert!(colored.contains(line));
    }

    #[test]
    fn test_colorize_plain() {
        let line = "listening on port 8080";
        let colored = colorize_log_line(line);
        assert_eq!(colored, line);
    }
}
