use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::Args;
use owo_colors::{OwoColorize, Stream, Style};
use serde::Serialize;
use tabled::{Table, Tabled, settings::Style as TableStyle};

use crate::scan::{
    Allowlist, Finding, SecretScanner, SkippedFile, install_pre_commit_hook, is_git_repo,
    scan_git_history, scan_staged_changes, scan_working_tree,
};
use crate::ui;

#[derive(Args)]
pub struct ScanArgs {
    /// Directory to scan
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// Install a git pre-commit hook that scans staged changes
    #[arg(long)]
    pub install_hook: bool,
    /// Skip git history and scan only the working tree
    #[arg(long)]
    pub no_history: bool,
    /// Scan only staged changes (used by git pre-commit hook)
    #[arg(long)]
    pub staged: bool,
    /// Output findings in JSON format
    #[arg(long)]
    pub json: bool,
    /// Automatically accept remediation offers (e.g. adding leaked files to .gitignore)
    #[arg(short, long)]
    pub yes: bool,
}

#[derive(Tabled)]
struct FindingRow {
    #[tabled(rename = "LOCATION")]
    location: String,
    #[tabled(rename = "RULE")]
    rule: String,
    #[tabled(rename = "MASKED SECRET")]
    masked: String,
}

#[derive(Serialize)]
struct ScanJsonReport {
    status: String,
    files_scanned: usize,
    findings: Vec<Finding>,
    skipped_files: Vec<SkippedFile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_warning: Option<String>,
}

pub async fn run(args: ScanArgs) -> Result<()> {
    let scan_dir = if args.path.is_relative() {
        std::env::current_dir()?.join(&args.path)
    } else {
        args.path.clone()
    };

    // 1. Handle --install-hook
    if args.install_hook {
        let hook_path = install_pre_commit_hook(&scan_dir)?;
        let display = hook_path.display();
        let ok_style = Style::new().green().bold();
        println!(
            "{} Pre-commit hook installed to `{display}`.",
            "✔".if_supports_color(Stream::Stdout, |t| t.style(ok_style))
        );
        return Ok(());
    }

    // 2. Load allowlist
    let allowlist_path = scan_dir.join(".asurignore");
    let allowlist = if allowlist_path.exists() {
        Allowlist::from_file(&allowlist_path)
    } else {
        Allowlist::new()
    };

    let scanner = SecretScanner::new();
    let mut findings: Vec<Finding> = Vec::new();
    let mut skipped_files: Vec<SkippedFile> = Vec::new();
    let mut files_scanned = 0usize;
    let mut git_warning: Option<String> = None;

    // 3. Perform scan
    if args.staged {
        findings = scan_staged_changes(&scan_dir, &scanner, &allowlist)?;
    } else {
        let wt_report = scan_working_tree(&scan_dir, &scanner, &allowlist)?;
        files_scanned = wt_report.files_scanned;
        findings.extend(wt_report.findings);
        skipped_files.extend(wt_report.skipped_files);

        // Scan git history if inside git repository and not disabled
        if !args.no_history && is_git_repo(&scan_dir) {
            match scan_git_history(&scan_dir, &scanner, &allowlist, Some(200)) {
                Ok(history_findings) => findings.extend(history_findings),
                Err(err) => {
                    git_warning = Some(format!("Git history scan could not be completed: {err}"));
                }
            }
        }
    }

    let has_incomplete_coverage = !skipped_files.is_empty() || git_warning.is_some();

    // 4. Handle JSON output
    if args.json {
        let status = if !findings.is_empty() {
            "secrets_found"
        } else if has_incomplete_coverage {
            "partial_clean"
        } else {
            "clean"
        };

        let report = ScanJsonReport {
            status: status.to_string(),
            files_scanned,
            findings: findings.clone(),
            skipped_files,
            git_warning,
        };

        let json_str = serde_json::to_string_pretty(&report)?;
        println!("{json_str}");

        if !findings.is_empty() {
            bail!(
                "Secrets detected during scan ({} finding(s))",
                findings.len()
            );
        }
        return Ok(());
    }

    // 5. Human-friendly output
    let ok_style = Style::new().green().bold();
    let err_style = Style::new().red().bold();
    let warn_style = Style::new().yellow().bold();

    // Display skipped files or warnings upfront
    if let Some(ref warn_msg) = git_warning {
        ui::warn(warn_msg);
    }

    if !skipped_files.is_empty() {
        ui::warn(&format!(
            "{} file(s) were skipped during scan:",
            skipped_files.len()
        ));
        for (i, sf) in skipped_files.iter().enumerate() {
            if i >= 10 {
                println!(
                    "    ... and {} more skipped file(s)",
                    skipped_files.len() - 10
                );
                break;
            }
            println!("    • {} ({})", sf.path, sf.reason);
        }
        println!();
    }

    if findings.is_empty() {
        if has_incomplete_coverage {
            println!(
                "{} No secrets detected in scanned files, but coverage was incomplete due to skipped files or git errors.",
                "⚠".if_supports_color(Stream::Stdout, |t| t.style(warn_style))
            );
        } else {
            println!(
                "{} Scan complete. No secrets detected ({} file(s) scanned).",
                "✔".if_supports_color(Stream::Stdout, |t| t.style(ok_style)),
                files_scanned
            );
        }
        return Ok(());
    }

    println!(
        "\n{} Found {} potential leaked secret(s):\n",
        "✖".if_supports_color(Stream::Stdout, |t| t.style(err_style)),
        findings.len()
    );

    let rows: Vec<FindingRow> = findings
        .iter()
        .map(|f| {
            let location = if let Some(ref commit) = f.commit {
                format!("commit {} ({}:{})", commit, f.file, f.line)
            } else {
                format!("{}:{}", f.file, f.line)
            };
            FindingRow {
                location,
                rule: f.rule.clone(),
                masked: f.masked.clone(),
            }
        })
        .collect();

    let table = Table::new(rows).with(TableStyle::sharp()).to_string();
    println!("{table}\n");

    // Warnings and remediation
    println!(
        "{} Remediation steps:",
        "›".if_supports_color(Stream::Stdout, |t| t.cyan())
    );
    println!(
        "  1. {}",
        "Rotate any exposed credentials immediately."
            .if_supports_color(Stream::Stdout, |t| t.style(warn_style))
    );
    println!("     Deleting or editing the file does NOT remove it from git commit history.");
    println!(
        "  2. To ignore false positives, add '# asur:ignore' inline or add patterns to `.asurignore`."
    );

    // Check if any working tree files with leaks should be added to .gitignore
    offer_gitignore_remediation(&scan_dir, &findings, args.yes)?;

    bail!(
        "Scan failed: {} leaked secret(s) found. Rotate exposed keys.",
        findings.len()
    );
}

fn offer_gitignore_remediation(
    repo_root: &Path,
    findings: &[Finding],
    auto_yes: bool,
) -> Result<()> {
    let gitignore_path = repo_root.join(".gitignore");

    // Collect files in working tree that have secrets
    let mut files_to_ignore: Vec<String> = findings
        .iter()
        .filter(|f| f.commit.is_none())
        .map(|f| f.file.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    files_to_ignore.sort();

    if files_to_ignore.is_empty() {
        return Ok(());
    }

    // Filter out files that are already listed in .gitignore
    let existing_content = std::fs::read_to_string(&gitignore_path).unwrap_or_default();
    files_to_ignore.retain(|file| !existing_content.lines().any(|line| line.trim() == file));

    if files_to_ignore.is_empty() {
        return Ok(());
    }

    println!();
    println!(
        "  3. Found {} unignored file(s) with secrets: {}",
        files_to_ignore.len(),
        files_to_ignore.join(", ")
    );

    let should_add = if auto_yes {
        true
    } else {
        inquire::Confirm::new("Add these file(s) to `.gitignore`?")
            .with_default(true)
            .prompt()
            .unwrap_or(false)
    };

    if should_add {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&gitignore_path)?;

        if !existing_content.is_empty() && !existing_content.ends_with('\n') {
            writeln!(file)?;
        }
        writeln!(file, "\n# Added by asur scan")?;
        for f in &files_to_ignore {
            writeln!(file, "{f}")?;
        }
        ui::info(&format!(
            "Added {} file(s) to `.gitignore`.",
            files_to_ignore.len()
        ));
    }

    Ok(())
}
