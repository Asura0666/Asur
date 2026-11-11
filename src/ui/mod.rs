//! Terminal output helpers.
//!
//! Colors are applied only when the stream supports them, so output stays clean
//! when piped and when `NO_COLOR` is set.

use owo_colors::{OwoColorize, Stream, Style};

pub fn error(msg: &str) {
    let style = Style::new().red().bold();
    eprintln!(
        "{} {msg}",
        "error:".if_supports_color(Stream::Stderr, |t| t.style(style))
    );
}

#[allow(dead_code)]
pub fn warn(msg: &str) {
    let style = Style::new().yellow().bold();
    eprintln!(
        "{} {msg}",
        "warning:".if_supports_color(Stream::Stderr, |t| t.style(style))
    );
}

pub fn info(msg: &str) {
    println!(
        "{} {msg}",
        "›".if_supports_color(Stream::Stdout, |t| t.cyan())
    );
}

/// Placeholder used by commands that are planned but not built yet.
pub fn not_implemented(command: &str, milestone: &str) {
    println!(
        "{} `{command}` is not implemented yet (planned for {milestone}, see PROJECT_BRIEF.md).",
        "…".if_supports_color(Stream::Stdout, |t| t.dimmed())
    );
}
