//! Formatting helpers for `asur ps`.
//!
//! All functions are pure (no I/O, no Docker) so they are easy to unit-test.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use owo_colors::{OwoColorize, Stream, Style};
use serde::{Deserialize, Serialize};
use tabled::{Table, settings::Style as TableStyle};

use crate::docker::ContainerInfo;

// ── JSON output type ──────────────────────────────────────────────────────────

/// Serialisable container record for `--json` output.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContainerJson {
    pub name: String,
    pub image: String,
    pub status: String,
    pub state: String,
    pub ports: Vec<String>,
    pub created: i64,
    pub uptime: String,
}

impl From<&ContainerInfo> for ContainerJson {
    fn from(c: &ContainerInfo) -> Self {
        Self {
            name: c.name.clone(),
            image: c.image.clone(),
            status: c.status.clone(),
            state: c.state.clone(),
            ports: c.ports.clone(),
            created: c.created,
            uptime: format_uptime(c.created),
        }
    }
}

// ── Table row ────────────────────────────────────────────────────────────────

/// One row in the `asur ps` table.
#[derive(tabled::Tabled)]
pub struct PsRow {
    #[tabled(rename = "NAME")]
    pub name: String,
    #[tabled(rename = "IMAGE")]
    pub image: String,
    #[tabled(rename = "STATUS")]
    pub status: String,
    #[tabled(rename = "PORTS")]
    pub ports: String,
    #[tabled(rename = "UPTIME")]
    pub uptime: String,
}

// ── Status colouring ─────────────────────────────────────────────────────────

/// Return a coloured status string when the terminal supports it.
///
/// - `running`  → green
/// - `paused`   → yellow
/// - `exited`   → red (dimmed)
/// - anything else → plain
pub fn colorize_status(state: &str, display: &str) -> String {
    match state.to_lowercase().as_str() {
        "running" => {
            let style = Style::new().green().bold();
            display
                .if_supports_color(Stream::Stdout, |t| t.style(style))
                .to_string()
        }
        "paused" => {
            let style = Style::new().yellow();
            display
                .if_supports_color(Stream::Stdout, |t| t.style(style))
                .to_string()
        }
        "restarting" => {
            let style = Style::new().yellow().bold();
            display
                .if_supports_color(Stream::Stdout, |t| t.style(style))
                .to_string()
        }
        "exited" | "dead" | "removing" | "created" => {
            let style = Style::new().red().dimmed();
            display
                .if_supports_color(Stream::Stdout, |t| t.style(style))
                .to_string()
        }
        _ => display.to_owned(),
    }
}

// ── Uptime formatting ────────────────────────────────────────────────────────

/// Format a Unix creation timestamp into a human-readable uptime string.
///
/// Returns `"–"` when the timestamp is zero or in the future.
pub fn format_uptime(created: i64) -> String {
    if created <= 0 {
        return "–".to_owned();
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64;

    let secs = now.saturating_sub(created);
    if secs < 2 {
        return "just now".to_owned();
    }

    let minutes = secs / 60;
    let hours = minutes / 60;
    let days = hours / 24;

    if days > 0 {
        let d = days;
        let h = hours % 24;
        if h == 0 {
            format!("{d}d")
        } else {
            format!("{d}d {h}h")
        }
    } else if hours > 0 {
        let h = hours;
        let m = minutes % 60;
        if m == 0 {
            format!("{h}h")
        } else {
            format!("{h}h {m}m")
        }
    } else if minutes > 0 {
        let m = minutes;
        let s = secs % 60;
        if s == 0 {
            format!("{m}m")
        } else {
            format!("{m}m {s}s")
        }
    } else {
        format!("{secs}s")
    }
}

// ── Table builder ────────────────────────────────────────────────────────────

/// Build a [`PsRow`] from a [`ContainerInfo`], colouring the status cell.
pub fn build_row(c: &ContainerInfo) -> PsRow {
    PsRow {
        name: c.name.clone(),
        image: c.image.clone(),
        status: colorize_status(&c.state, &c.status),
        ports: c.ports.join(", "),
        uptime: format_uptime(c.created),
    }
}

/// Render a table of containers.  Returns the formatted string (no newline at end).
///
/// When `containers` is empty the returned string is an empty-list notice.
pub fn render_table(containers: &[ContainerInfo]) -> String {
    if containers.is_empty() {
        return format!(
            "{} No containers found. Run `asur up <service>` to start one.",
            "›".if_supports_color(Stream::Stdout, |t| t.cyan())
        );
    }

    let rows: Vec<PsRow> = containers.iter().map(build_row).collect();
    let mut table = Table::new(rows);
    table.with(TableStyle::sharp());

    table.to_string()
}

/// Render containers as a JSON array.
pub fn render_json(containers: &[ContainerInfo]) -> anyhow::Result<String> {
    let records: Vec<ContainerJson> = containers.iter().map(ContainerJson::from).collect();
    serde_json::to_string_pretty(&records).map_err(anyhow::Error::from)
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_container(state: &str, status: &str, created: i64, ports: Vec<&str>) -> ContainerInfo {
        ContainerInfo {
            name: "test-container".into(),
            image: "my-image:latest".into(),
            status: status.into(),
            state: state.into(),
            ports: ports.into_iter().map(str::to_owned).collect(),
            created,
        }
    }

    // ── colorize_status ──────────────────────────────────────────────────────

    #[test]
    fn running_state_contains_status_text() {
        let s = colorize_status("running", "Up 2 hours");
        assert!(s.contains("Up 2 hours"), "got: {s:?}");
    }

    #[test]
    fn exited_state_contains_status_text() {
        let s = colorize_status("exited", "Exited (0) 1 day ago");
        assert!(s.contains("Exited (0) 1 day ago"), "got: {s:?}");
    }

    #[test]
    fn unknown_state_is_plain() {
        // No colour codes added for unknown states.
        let s = colorize_status("weird", "Doing stuff");
        assert_eq!(s, "Doing stuff");
    }

    // ── format_uptime ────────────────────────────────────────────────────────

    #[test]
    fn zero_created_returns_dash() {
        assert_eq!(format_uptime(0), "–");
    }

    #[test]
    fn negative_created_returns_dash() {
        assert_eq!(format_uptime(-1), "–");
    }

    #[test]
    fn recent_creation_shows_seconds() {
        // A container created 45 seconds ago.
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let uptime = format_uptime(now_secs - 45);
        assert!(
            uptime.ends_with('s') || uptime == "just now",
            "expected seconds, got {uptime:?}"
        );
    }

    #[test]
    fn hours_and_minutes() {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        // 2 hours 30 minutes ago
        let created = now_secs - (2 * 3600 + 30 * 60);
        let uptime = format_uptime(created);
        assert_eq!(uptime, "2h 30m", "got: {uptime:?}");
    }

    #[test]
    fn exact_hours_no_minutes() {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let created = now_secs - (3 * 3600);
        let uptime = format_uptime(created);
        assert_eq!(uptime, "3h", "got: {uptime:?}");
    }

    #[test]
    fn days_and_hours() {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let created = now_secs - (86400 + 5 * 3600);
        let uptime = format_uptime(created);
        assert_eq!(uptime, "1d 5h", "got: {uptime:?}");
    }

    // ── render_table ─────────────────────────────────────────────────────────

    #[test]
    fn empty_list_message() {
        let output = render_table(&[]);
        assert!(output.contains("No containers found"), "got: {output:?}");
        assert!(output.contains("asur up"), "hint missing, got: {output:?}");
    }

    #[test]
    fn table_contains_container_name() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let c = fake_container("running", "Up 1 hour", now - 3600, vec!["5432/TCP"]);
        let table = render_table(&[c]);
        assert!(table.contains("test-container"), "got:\n{table}");
        assert!(table.contains("my-image:latest"), "got:\n{table}");
        assert!(table.contains("5432/TCP"), "got:\n{table}");
    }

    #[test]
    fn table_contains_multiple_containers() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let mut c1 = fake_container("running", "Up 2 hours", now - 7200, vec![]);
        c1.name = "redis".into();

        let mut c2 = fake_container("exited", "Exited (0) 1 hour ago", now - 3600, vec![]);
        c2.name = "postgres".into();

        let table = render_table(&[c1, c2]);
        assert!(table.contains("redis"), "got:\n{table}");
        assert!(table.contains("postgres"), "got:\n{table}");
    }

    // ── render_json ──────────────────────────────────────────────────────────

    #[test]
    fn json_empty_list_is_array() {
        let json = render_json(&[]).unwrap();
        assert_eq!(json.trim(), "[]");
    }

    #[test]
    fn json_contains_expected_fields() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let c = fake_container(
            "running",
            "Up 5 minutes",
            now - 300,
            vec!["0.0.0.0:6379->6379/TCP"],
        );
        let json = render_json(&[c]).unwrap();

        // Deserialise and inspect.
        let parsed: Vec<ContainerJson> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 1);
        let r = &parsed[0];
        assert_eq!(r.name, "test-container");
        assert_eq!(r.state, "running");
        assert_eq!(r.ports, vec!["0.0.0.0:6379->6379/TCP"]);
        assert!(!r.uptime.is_empty());
    }

    #[test]
    fn json_no_color_codes() {
        // JSON must never contain ANSI escape sequences.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let c = fake_container("running", "Up 1 hour", now - 3600, vec![]);
        let json = render_json(&[c]).unwrap();
        assert!(
            !json.contains('\x1b'),
            "ANSI escape in JSON output:\n{json}"
        );
    }
}
