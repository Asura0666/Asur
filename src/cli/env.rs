//! Work with .env files: generate .env.example, diff, validate (`asur env`).

use std::fs;
use std::io::IsTerminal;
use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use owo_colors::{OwoColorize, Stream, Style};

use crate::env::{EnvFile, diff_env, generate_example, validate_env};
use crate::ui;

#[derive(Args, Debug, Clone)]
pub struct EnvArgs {
    #[command(subcommand)]
    pub command: EnvCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum EnvCommand {
    /// Generate .env.example from .env with secret values stripped
    Example {
        /// Source .env file
        #[arg(short, long, default_value = ".env")]
        from: String,
        /// Target example file
        #[arg(short, long, default_value = ".env.example")]
        to: String,
        /// Overwrite without asking
        #[arg(short, long)]
        yes: bool,
    },
    /// Compare .env and .env.example
    Diff {
        /// Path to .env file
        #[arg(short, long, default_value = ".env")]
        env: String,
        /// Path to .env.example file
        #[arg(short, long, default_value = ".env.example")]
        example: String,
    },
    /// Validate required keys and value types
    Check {
        /// Path to .env file
        #[arg(short, long, default_value = ".env")]
        env: String,
        /// Path to .env.example file to verify required keys
        #[arg(short, long, default_value = ".env.example")]
        example: String,
    },
    /// Install a git pre-commit hook that prevents accidentally committing .env files
    Hook,
}

pub async fn run(args: EnvArgs) -> Result<()> {
    match args.command {
        EnvCommand::Example { from, to, yes } => handle_example(&from, &to, yes),
        EnvCommand::Diff { env, example } => handle_diff(&env, &example),
        EnvCommand::Check { env, example } => handle_check(&env, &example),
        EnvCommand::Hook => handle_hook(),
    }
}

fn handle_example(from: &str, to: &str, yes: bool) -> Result<()> {
    if !Path::new(from).exists() {
        bail!("source file `{from}` does not exist. Make sure you run this in your project root.");
    }

    let content = fs::read_to_string(from).context(format!("failed to read `{from}`"))?;
    let env_file = EnvFile::parse(&content);
    let example_content = generate_example(&env_file);

    if Path::new(to).exists() && !yes && std::io::stdin().is_terminal() {
        let confirmed = inquire::Confirm::new(&format!("`{to}` already exists. Overwrite?"))
            .with_default(false)
            .prompt()
            .unwrap_or(false);

        if !confirmed {
            ui::info("Cancelled.");
            return Ok(());
        }
    }

    fs::write(to, example_content).context(format!("failed to write `{to}`"))?;

    let green_bold = Style::new().green().bold();
    let check = "✔".if_supports_color(Stream::Stdout, |t| t.style(green_bold));
    println!(
        "\n{check} Generated `{to}` with {} key(s) (sensitive values stripped).\n",
        env_file.keys().len()
    );

    Ok(())
}

fn handle_diff(env_path: &str, example_path: &str) -> Result<()> {
    if !Path::new(env_path).exists() {
        bail!("`{env_path}` does not exist.");
    }
    if !Path::new(example_path).exists() {
        bail!("`{example_path}` does not exist.");
    }

    let env_content =
        fs::read_to_string(env_path).context(format!("failed to read `{env_path}`"))?;
    let example_content =
        fs::read_to_string(example_path).context(format!("failed to read `{example_path}`"))?;

    let env_file = EnvFile::parse(&env_content);
    let example_file = EnvFile::parse(&example_content);

    let report = diff_env(&env_file, &example_file);

    println!("\nComparing `{env_path}` vs `{example_path}`:\n");

    if report.missing_in_env.is_empty() && report.extra_in_env.is_empty() {
        let green_bold = Style::new().green().bold();
        let check = "✔".if_supports_color(Stream::Stdout, |t| t.style(green_bold));
        println!(
            "{check} Perfect match! All {} key(s) are documented and present in both files.\n",
            report.matching_keys.len()
        );
        return Ok(());
    }

    if !report.missing_in_env.is_empty() {
        let red_bold = Style::new().red().bold();
        let warn_icon = "✖".if_supports_color(Stream::Stdout, |t| t.style(red_bold));
        println!("{warn_icon} Missing in `{env_path}` (present in `{example_path}`):");
        for k in &report.missing_in_env {
            println!("  - {k}");
        }
        println!();
    }

    if !report.extra_in_env.is_empty() {
        let yellow = Style::new().yellow();
        let plus_icon = "+".if_supports_color(Stream::Stdout, |t| t.style(yellow));
        println!("{plus_icon} Undocumented in `{example_path}` (present in `{env_path}`):");
        for k in &report.extra_in_env {
            println!("  + {k}");
        }
        println!();
    }

    println!("  In sync: {} key(s).\n", report.matching_keys.len());
    Ok(())
}

fn handle_check(env_path: &str, example_path: &str) -> Result<()> {
    if !Path::new(env_path).exists() {
        bail!("`{env_path}` does not exist.");
    }

    let env_content =
        fs::read_to_string(env_path).context(format!("failed to read `{env_path}`"))?;
    let env_file = EnvFile::parse(&env_content);

    let type_errors = validate_env(&env_file);

    // If example file exists, check for missing keys as well
    let mut missing_keys = Vec::new();
    if Path::new(example_path).exists() {
        if let Ok(example_content) = fs::read_to_string(example_path) {
            let example_file = EnvFile::parse(&example_content);
            let diff = diff_env(&env_file, &example_file);
            missing_keys = diff.missing_in_env;
        }
    }

    if type_errors.is_empty() && missing_keys.is_empty() {
        let green_bold = Style::new().green().bold();
        let check = "✔".if_supports_color(Stream::Stdout, |t| t.style(green_bold));
        println!(
            "\n{check} All {} environment variable(s) passed validation!\n",
            env_file.keys().len()
        );
        return Ok(());
    }

    println!("\nValidation issues found in `{env_path}`:\n");

    if !missing_keys.is_empty() {
        let red_bold = Style::new().red().bold();
        let icon = "✖".if_supports_color(Stream::Stdout, |t| t.style(red_bold));
        println!("{icon} Missing required variable(s) from `{example_path}`:");
        for k in &missing_keys {
            println!("  • {k}");
        }
        println!();
    }

    if !type_errors.is_empty() {
        let red_bold = Style::new().red().bold();
        let icon = "✖".if_supports_color(Stream::Stdout, |t| t.style(red_bold));
        println!("{icon} Value type error(s):");
        for err in &type_errors {
            println!("  • {}: {}", err.key, err.message);
        }
        println!();
    }

    bail!("environment validation failed.");
}

fn handle_hook() -> Result<()> {
    let git_dir = Path::new(".git");
    if !git_dir.exists() {
        bail!("no `.git` directory found. Run this from the root of a Git repository.");
    }

    let hooks_dir = git_dir.join("hooks");
    fs::create_dir_all(&hooks_dir).context("failed to create hooks directory")?;

    let pre_commit_path = hooks_dir.join("pre-commit");

    let hook_script = r#"#!/bin/sh
# asur pre-commit hook: block committing .env files
if git diff --cached --name-only | grep -E '^(.*/)?\.env(\..+)?$' | grep -v '\.env\.example$'; then
    echo "\033[1;31m[asur] ERROR: Attempting to commit a .env file containing potential secrets!\033[0m"
    echo "[asur] Please unstage it with: git reset HEAD <file>"
    echo "[asur] Only .env.example should be committed."
    exit 1
fi
"#;

    if pre_commit_path.exists() {
        let existing = fs::read_to_string(&pre_commit_path)?;
        if existing.contains("asur pre-commit hook") {
            ui::info("asur pre-commit hook is already installed.");
            return Ok(());
        }
        // Append to existing hook
        let combined = format!("{existing}\n\n{hook_script}");
        fs::write(&pre_commit_path, combined)?;
    } else {
        fs::write(&pre_commit_path, hook_script)?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&pre_commit_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&pre_commit_path, perms)?;
    }

    let green_bold = Style::new().green().bold();
    let check = "✔".if_supports_color(Stream::Stdout, |t| t.style(green_bold));
    println!(
        "\n{check} Installed git pre-commit hook at `{}`.\n",
        pre_commit_path.display()
    );
    Ok(())
}
