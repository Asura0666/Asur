//! Inspect and free ports (`asur port`).

use std::io::IsTerminal;
use std::process::Command;

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use owo_colors::{OwoColorize, Stream, Style};

use crate::docker::{ContainerRuntime, DockerRuntime};
use crate::ui;
use crate::util::clipboard::copy_to_clipboard;
use crate::util::ports::{extract_docker_ports, find_available_port, is_port_free};

/// `asur port 3000` shows who uses a port; `asur port free 3000` finds the next free one.
#[derive(Args, Debug, Clone, Default)]
#[command(args_conflicts_with_subcommands = true)]
pub struct PortArgs {
    /// Port to inspect
    pub port: Option<u16>,
    /// Terminate the process or stop the container using the port
    #[arg(short, long)]
    pub kill: bool,
    /// Skip confirmation prompt when killing/stopping
    #[arg(short, long)]
    pub yes: bool,
    #[command(subcommand)]
    pub command: Option<PortCommand>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum PortCommand {
    /// Print the next free port at or above the given one
    Free {
        /// Port to start searching from
        start: u16,
        /// Copy the result to the clipboard
        #[arg(long)]
        copy: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub user: Option<String>,
}

pub async fn run(args: PortArgs) -> Result<()> {
    let runtime = DockerRuntime::connect().ok();
    execute(args, runtime.as_ref()).await
}

pub async fn execute<R: ContainerRuntime>(args: PortArgs, runtime: Option<&R>) -> Result<()> {
    match (args.command, args.port) {
        (Some(PortCommand::Free { start, copy }), _) => {
            handle_port_free(start, copy, runtime).await
        }
        (None, Some(port)) => handle_port_inspect(port, args.kill, args.yes, runtime).await,
        (None, None) => {
            bail!("give a port to inspect (e.g. `asur port 3000`) or use `asur port free 3000`");
        }
    }
}

/// Find and print the next free port, optionally copying to clipboard.
async fn handle_port_free<R: ContainerRuntime>(
    start: u16,
    copy: bool,
    runtime: Option<&R>,
) -> Result<()> {
    let containers = if let Some(rt) = runtime {
        rt.list_containers(true).await.unwrap_or_default()
    } else {
        Vec::new()
    };

    let docker_ports = extract_docker_ports(&containers);
    let free_port = find_available_port(start, &docker_ports);

    let green_bold = Style::new().green().bold();
    let port_str = free_port.to_string();
    let port_styled = port_str.if_supports_color(Stream::Stdout, |t| t.style(green_bold));

    if copy {
        let copied = copy_to_clipboard(&port_str);
        if copied {
            println!(
                "Next free port: {port_styled} {}",
                "(✔ copied to clipboard)".if_supports_color(Stream::Stdout, |t| t.green())
            );
        } else {
            println!("Next free port: {port_styled}");
        }
    } else {
        println!("Next free port: {port_styled}");
    }

    Ok(())
}

/// Inspect who is using a port (Docker container or OS process) and offer to stop/kill.
async fn handle_port_inspect<R: ContainerRuntime>(
    port: u16,
    kill: bool,
    yes: bool,
    runtime: Option<&R>,
) -> Result<()> {
    let is_interactive = std::io::stdin().is_terminal();
    let containers = if let Some(rt) = runtime {
        rt.list_containers(true).await.unwrap_or_default()
    } else {
        Vec::new()
    };

    let docker_ports = extract_docker_ports(&containers);

    // 1. Check if a Docker container owns the port
    if let Some(container_name) = docker_ports.get(&port) {
        let container_info = containers.iter().find(|c| c.name == *container_name);
        let image = container_info
            .map(|c| c.image.as_str())
            .unwrap_or("unknown");
        let status = container_info
            .map(|c| c.status.as_str())
            .unwrap_or("unknown");

        let bold = Style::new().bold();
        let yellow = Style::new().yellow().bold();
        println!(
            "\nPort {} is in use by Docker container `{}`.\n",
            port.if_supports_color(Stream::Stdout, |t| t.style(yellow)),
            container_name.if_supports_color(Stream::Stdout, |t| t.style(bold))
        );
        println!("  Container: {container_name}");
        println!("  Image    : {image}");
        println!("  Status   : {status}\n");

        if !kill {
            println!("  › To stop this container, run: `asur port {port} --kill`");
            return Ok(());
        }

        let should_stop = if yes {
            true
        } else if !is_interactive {
            ui::warn(
                "Non-interactive terminal detected: pass `--yes` with `--kill` to authorize stopping the container.",
            );
            false
        } else {
            inquire::Confirm::new(&format!(
                "Stop Docker container `{container_name}` to free port {port}?"
            ))
            .with_default(false)
            .prompt()
            .unwrap_or(false)
        };

        if should_stop {
            if let Some(rt) = runtime {
                ui::info(&format!("Stopping `{container_name}`..."));
                rt.stop_container(container_name).await?;
                let green_bold = Style::new().green().bold();
                let check = "✔".if_supports_color(Stream::Stdout, |t| t.style(green_bold));
                println!(
                    "  {check} Stopped container `{container_name}`. Port {port} is now free!\n"
                );
            } else {
                ui::warn("Could not connect to Docker daemon to stop the container.");
            }
        } else {
            ui::info("Container left running.");
        }

        return Ok(());
    }

    // 2. Check if a host process owns the port
    if let Some(proc) = find_process_on_port(port)? {
        let bold = Style::new().bold();
        let yellow = Style::new().yellow().bold();
        println!(
            "\nPort {} is in use by process `{}` (PID {}).\n",
            port.if_supports_color(Stream::Stdout, |t| t.style(yellow)),
            proc.name
                .if_supports_color(Stream::Stdout, |t| t.style(bold)),
            proc.pid
        );
        println!("  PID  : {}", proc.pid);
        println!("  Name : {}", proc.name);
        if let Some(user) = &proc.user {
            println!("  User : {user}");
        }
        println!();

        if !kill {
            println!("  › To terminate this process, run: `asur port {port} --kill`");
            return Ok(());
        }

        let should_kill = if yes {
            true
        } else if !is_interactive {
            ui::warn(
                "Non-interactive terminal detected: pass `--yes` with `--kill` to authorize terminating the process.",
            );
            false
        } else {
            inquire::Confirm::new(&format!(
                "Kill process `{}` (PID {}) to free port {port}?",
                proc.name, proc.pid
            ))
            .with_default(false)
            .prompt()
            .unwrap_or(false)
        };

        if should_kill {
            ui::info(&format!(
                "Killing process `{}` (PID {})...",
                proc.name, proc.pid
            ));
            kill_process(proc.pid)?;
            let green_bold = Style::new().green().bold();
            let check = "✔".if_supports_color(Stream::Stdout, |t| t.style(green_bold));
            println!(
                "  {check} Terminated process {}. Port {port} is now free!\n",
                proc.pid
            );
        } else {
            ui::info("Process left running.");
        }

        return Ok(());
    }

    // 3. Port is free or unable to detect
    if is_port_free(port) {
        println!(
            "Port {} is free! No active process or container is using it.",
            port
        );
    } else {
        println!(
            "Port {port} is in use, but process information could not be determined.\n\
             Tip: Run your terminal as Administrator / sudo to view system sockets."
        );
    }

    Ok(())
}

/// Find which process is listening on the given TCP port.
pub fn find_process_on_port(port: u16) -> Result<Option<ProcessInfo>> {
    #[cfg(windows)]
    {
        find_process_on_port_windows(port)
    }

    #[cfg(not(windows))]
    {
        find_process_on_port_unix(port)
    }
}

/// Windows implementation: uses `netstat -ano -p tcp` and `tasklist`.
#[cfg(windows)]
fn find_process_on_port_windows(port: u16) -> Result<Option<ProcessInfo>> {
    let output = Command::new("netstat").args(["-ano", "-p", "tcp"]).output();

    let out = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return Ok(None),
    };

    let pid = match parse_netstat_output(&out, port) {
        Some(p) => p,
        None => return Ok(None),
    };

    let name = get_process_name_windows(pid).unwrap_or_else(|| "unknown".to_string());
    Ok(Some(ProcessInfo {
        pid,
        name,
        user: None,
    }))
}

/// Look up executable name for a given PID using `tasklist` on Windows.
#[cfg(windows)]
fn get_process_name_windows(pid: u32) -> Option<String> {
    let output = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next()?.trim();
    if line.is_empty() || line.starts_with("INFO:") {
        return None;
    }

    // CSV format: "Image Name","PID",...
    let name = line.split(',').next()?.trim_matches('"');
    Some(name.to_string())
}

/// Parse Windows `netstat -ano -p tcp` output to find the PID listening on `target_port`.
pub fn parse_netstat_output(output: &str, target_port: u16) -> Option<u32> {
    let port_suffix = format!(":{target_port}");

    for line in output.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        // Format: TCP <local_address> <foreign_address> <state> <pid>
        if parts.len() >= 5 && parts[0].eq_ignore_ascii_case("tcp") {
            let local_addr = parts[1];
            let state = parts[3];
            let pid_str = parts[4];

            if local_addr.ends_with(&port_suffix) && state.eq_ignore_ascii_case("listening") {
                if let Ok(pid) = pid_str.parse::<u32>() {
                    return Some(pid);
                }
            }
        }
    }

    None
}

/// Unix implementation: uses `lsof -n -P -iTCP:<port> -sTCP:LISTEN`.
#[cfg(not(windows))]
fn find_process_on_port_unix(port: u16) -> Result<Option<ProcessInfo>> {
    let output = Command::new("lsof")
        .args(["-n", "-P", &format!("-iTCP:{port}"), "-sTCP:LISTEN"])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            Ok(parse_lsof_output(&text))
        }
        _ => Ok(None),
    }
}

/// Parse `lsof` table output.
pub fn parse_lsof_output(output: &str) -> Option<ProcessInfo> {
    let mut lines = output.lines();
    let _header = lines.next()?;
    let data_line = lines.next()?;
    let parts: Vec<&str> = data_line.split_whitespace().collect();

    // Expected: COMMAND PID USER ...
    if parts.len() >= 3 {
        let name = parts[0].to_string();
        let pid = parts[1].parse::<u32>().ok()?;
        let user = Some(parts[2].to_string());
        Some(ProcessInfo { pid, name, user })
    } else {
        None
    }
}

/// Kill a process by PID.
pub fn kill_process(pid: u32) -> Result<()> {
    #[cfg(windows)]
    {
        let out = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output()?;

        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            if err.to_lowercase().contains("access is denied") {
                bail!("access denied. Run your terminal as Administrator to terminate PID {pid}.");
            } else {
                bail!("failed to terminate process {pid}: {err}");
            }
        }
    }

    #[cfg(not(windows))]
    {
        let out = Command::new("kill")
            .args(["-9", &pid.to_string()])
            .output()?;

        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            if err.to_lowercase().contains("operation not permitted") {
                bail!("permission denied. Run with sudo to terminate PID {pid}.");
            } else {
                bail!("failed to terminate process {pid}: {err}");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_netstat_listening() {
        let sample = "
Active Connections

  Proto  Local Address          Foreign Address        State           PID
  TCP    0.0.0.0:135            0.0.0.0:0              LISTENING       1124
  TCP    0.0.0.0:5432           0.0.0.0:0              LISTENING       4560
  TCP    127.0.0.1:6379         0.0.0.0:0              LISTENING       7890
";
        assert_eq!(parse_netstat_output(sample, 5432), Some(4560));
        assert_eq!(parse_netstat_output(sample, 6379), Some(7890));
        assert_eq!(parse_netstat_output(sample, 8080), None);
    }

    #[test]
    fn test_parse_lsof() {
        let sample = "COMMAND   PID USER   FD   TYPE DEVICE SIZE/OFF NODE NAME\nnode    12345 user   22u  IPv6 0x123      0t0  TCP *:3000 (LISTEN)\n";
        let info = parse_lsof_output(sample).unwrap();
        assert_eq!(info.pid, 12345);
        assert_eq!(info.name, "node");
        assert_eq!(info.user, Some("user".to_string()));
    }
}
