//! Shared terminal presentation for every Asur command.

use std::io::{self, IsTerminal};
use std::sync::{OnceLock, RwLock};

use crossterm::terminal;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use owo_colors::OwoColorize;

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    pub plain: bool,
    pub no_color: bool,
    pub no_progress: bool,
}

/// The renderer selected for this invocation.  Rich output is deliberately an
/// enhancement: redirected output and terminals that cannot report a size use
/// stable, line-oriented messages instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    Rich,
    Lines,
    Plain,
}

static OPTIONS: OnceLock<RwLock<Options>> = OnceLock::new();

pub fn configure(plain: bool, no_color: bool, no_progress: bool) {
    let options = OPTIONS.get_or_init(|| RwLock::new(Options::default()));
    if let Ok(mut current) = options.write() {
        *current = Options {
            plain,
            no_color,
            no_progress,
        };
    }

    // Inquire's default theme uses cyan for answers and highlighted options.
    // Keep prompts consistent with Asur's green/neutral palette and preserve a
    // completely unstyled mode for --no-color, --plain, and NO_COLOR.
    let no_color = plain || no_color || std::env::var_os("NO_COLOR").is_some();
    let prompt_config = if no_color {
        inquire::ui::RenderConfig::empty()
    } else {
        use inquire::ui::{Color, StyleSheet, Styled};

        let mut config = inquire::ui::RenderConfig::default_colored();
        config.answer = StyleSheet::new().with_fg(Color::LightGreen);
        config.highlighted_option_prefix = Styled::new(">").with_fg(Color::LightGreen);
        config.selected_option = Some(StyleSheet::new().with_fg(Color::LightGreen));
        config.help_message = StyleSheet::new().with_fg(Color::DarkGrey);
        config
    };
    inquire::set_global_render_config(prompt_config);
}

fn options() -> Options {
    OPTIONS
        .get()
        .and_then(|value| value.read().ok().map(|value| *value))
        .unwrap_or_default()
}

pub fn render_mode() -> RenderMode {
    let current = options();
    if current.plain {
        RenderMode::Plain
    } else if current.no_progress || !io::stderr().is_terminal() || terminal::size().is_err() {
        RenderMode::Lines
    } else {
        RenderMode::Rich
    }
}

fn rich() -> bool {
    render_mode() == RenderMode::Rich
}

fn color() -> bool {
    let current = options();
    !current.plain && !current.no_color
}

pub fn header(command: &str, context: Option<&str>) {
    if options().plain {
        return;
    }
    let title = context.map_or_else(
        || format!("ASUR / {command}"),
        |value| format!("ASUR / {command} / {value}"),
    );
    if color() {
        println!("{}", title.green().bold());
    } else {
        println!("{title}");
    }
}
pub fn section(title: &str) {
    if !options().plain {
        println!("\n── {title} ─────────────────────────────────────────");
    }
}
pub fn success(msg: &str) {
    println!(
        "{} {msg}",
        if color() {
            "✓".to_string()
        } else {
            "OK".to_string()
        }
    );
}
pub fn error(msg: &str) {
    eprintln!("error: {msg}");
}
pub fn warn(msg: &str) {
    eprintln!("warning: {msg}");
}
pub fn info(msg: &str) {
    println!("› {msg}");
}
pub fn hint(msg: &str) {
    println!("  {msg}");
}

/// Print aligned fields used by command summaries without giving each command
/// its own spacing rules.
pub fn key_values(values: &[(&str, String)]) {
    let width = values.iter().map(|(key, _)| key.len()).max().unwrap_or(0);
    for (key, value) in values {
        println!("  {key:width$} : {value}");
    }
}

/// An elapsed-time indicator for actual indeterminate work.
pub struct Spinner {
    progress: Option<ProgressBar>,
}
impl Spinner {
    pub fn new(stage: impl AsRef<str>) -> Self {
        if !rich() {
            info(stage.as_ref());
            return Self { progress: None };
        }
        let progress = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr());
        progress.set_style(
            ProgressStyle::with_template("{spinner:.green} {msg} [{elapsed_precise}]")
                .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        progress.enable_steady_tick(std::time::Duration::from_millis(100));
        progress.set_message(stage.as_ref().to_owned());
        Self {
            progress: Some(progress),
        }
    }
    pub fn set_message(&self, message: impl Into<String>) {
        if let Some(progress) = &self.progress {
            progress.set_message(message.into());
        }
    }
    pub fn finish(&self, message: impl AsRef<str>) {
        if let Some(progress) = &self.progress {
            progress.finish_with_message(message.as_ref().to_owned());
        } else {
            success(message.as_ref());
        }
    }

    pub fn clear(&self) {
        if let Some(progress) = &self.progress {
            progress.finish_and_clear();
        }
    }
}

/// A determinate indicator.  Call this only when the total is authoritative;
/// callers should use [`Spinner`] for status-only work.
pub struct Progress {
    progress: Option<ProgressBar>,
}

impl Progress {
    pub fn new(label: impl Into<String>, total: u64) -> Self {
        let label = label.into();
        if !rich() || total == 0 {
            info(&format!("{label}: 0/{total}"));
            return Self { progress: None };
        }
        let progress = ProgressBar::with_draw_target(Some(total), ProgressDrawTarget::stderr());
        progress.set_style(
            ProgressStyle::with_template(
                "{msg:.green} [{bar:30.green/white}] {pos}% [{elapsed_precise}]",
            )
            .unwrap_or_else(|_| ProgressStyle::default_bar()),
        );
        progress.set_message(label.clone());
        Self {
            progress: Some(progress),
        }
    }

    /// Render a stable percentage instead of raw Docker layer counters.
    pub fn set_percent(&self, percent: u64) {
        if let Some(progress) = &self.progress {
            progress.set_length(100);
            progress.set_position(percent.min(100));
        }
    }

    pub fn finish(&self, message: impl AsRef<str>) {
        if let Some(progress) = &self.progress {
            // A byte bar represents live state.  Keeping it at 100% in the
            // scrollback makes the next launch look pre-completed; its caller
            // emits the durable ready/error summary immediately afterwards.
            progress.finish_and_clear();
        } else {
            success(message.as_ref());
        }
    }

    pub fn clear(&self) {
        if let Some(progress) = &self.progress {
            progress.finish_and_clear();
        }
    }
}

pub fn not_implemented(command: &str, milestone: &str) {
    info(&format!(
        "`{command}` is not implemented yet (planned for {milestone})."
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static UI_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        UI_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn explicit_plain_mode_wins() {
        let _guard = test_guard();
        configure(true, false, false);
        assert_eq!(render_mode(), RenderMode::Plain);
    }

    #[test]
    fn no_progress_uses_line_renderer() {
        let _guard = test_guard();
        configure(false, false, true);
        assert_eq!(render_mode(), RenderMode::Lines);
    }

    #[test]
    fn progress_accepts_authoritative_total() {
        let _guard = test_guard();
        configure(false, false, true);
        let progress = Progress::new("Downloading image", 42);
        progress.set_percent(42);
        progress.finish("Image downloaded.");
    }
}
