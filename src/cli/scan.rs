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
    Allowlist, Finding, GitRepoStatus, SecretScanner, SkippedFile, check_git_repo_status,
    install_pre_commit_hook, scan_git_history, scan_staged_changes,
    scan_working_tree_with_progress,
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
    /// Allow partial scan to succeed (exit code 0) even if files were skipped or git history could not be verified
    #[arg(long)]
    pub allow_partial: bool,
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
    if !args.json {
        ui::header("scan", args.path.to_str());
    }
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
        let scan_spinner = if args.json {
            None
        } else {
            Some(ui::Spinner::new(
                "Scanning working tree (streaming traversal)...",
            ))
        };
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();
        let scan_root = scan_dir.clone();
        let scan_scanner = scanner.clone();
        let scan_allowlist = allowlist.clone();
        let scan_task = tokio::task::spawn_blocking(move || {
            scan_working_tree_with_progress(
                &scan_root,
                &scan_scanner,
                &scan_allowlist,
                |progress| {
                    if progress.files_discovered % 100 == 0 {
                        let _ = progress_tx.send(progress);
                    }
                    true
                },
            )
        });
        while let Some(progress) = progress_rx.recv().await {
            if let Some(spinner) = &scan_spinner {
                spinner.set_message(format!(
                    "Scanning: {} directories, {} discovered, {} scanned, {} skipped{}",
                    progress.directories_visited,
                    progress.files_discovered,
                    progress.files_scanned,
                    progress.files_skipped,
                    progress
                        .current_path
                        .as_deref()
                        .map(|path| format!(" ({path})"))
                        .unwrap_or_default()
                ));
            }
        }
        let wt_report = scan_task
            .await
            .map_err(|error| anyhow::anyhow!("scan worker failed: {error}"))??;
        if let Some(spinner) = &scan_spinner {
            spinner.finish(format!(
                "Working tree complete: {} directories, {} discovered, {} scanned, {} skipped.",
                wt_report.directories_visited,
                wt_report.files_discovered,
                wt_report.files_scanned,
                wt_report.skipped_files.len()
            ));
        }
        files_scanned = wt_report.files_scanned;
        findings.extend(wt_report.findings);
        skipped_files.extend(wt_report.skipped_files);

        // Scan git history if inside git repository and not disabled
        if !args.no_history {
            match check_git_repo_status(&scan_dir) {
                GitRepoStatus::InsideWorkTree => {
                    let history_spinner = ui::Spinner::new("Scanning Git history...");
                    match scan_git_history(&scan_dir, &scanner, &allowlist, Some(200)) {
                        Ok(history_findings) => findings.extend(history_findings),
                        Err(err) => {
                            git_warning =
                                Some(format!("Git history scan could not be completed: {err}"));
                        }
                    }
                    history_spinner.finish("Git history scan complete.");
                }
                GitRepoStatus::GitUnavailable(reason) => {
                    git_warning = Some(format!("Git history could not be verified: {reason}"));
                }
                GitRepoStatus::NotGitRepo => {
                    // Regular directory, no git history to scan
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
            skipped_files: skipped_files.clone(),
            git_warning: git_warning.clone(),
        };

        let json_str = serde_json::to_string_pretty(&report)?;
        println!("{json_str}");

        if !findings.is_empty() {
            bail!(
                "Secrets detected during scan ({} finding(s))",
                findings.len()
            );
        }

        if has_incomplete_coverage && !args.allow_partial {
            bail!(
                "Scan incomplete: coverage was limited by skipped files or git errors. Pass `--allow-partial` to permit partial scans."
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
            if !args.allow_partial {
                bail!(
                    "Scan incomplete: {} file(s) skipped or git error encountered. Pass `--allow-partial` to permit partial scans.",
                    skipped_files.len()
                );
            }
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
        "›".if_supports_color(Stream::Stdout, |t| t.green())
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
            .map_err(super::prompt_error)?
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_scan_clean_dir_succeeds() {
        let temp_dir =
            std::env::temp_dir().join(format!("asur_test_scan_clean_{}", rand::random::<u64>()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("clean.txt"), "hello world").unwrap();

        let args = ScanArgs {
            path: temp_dir.clone(),
            install_hook: false,
            no_history: true,
            staged: false,
            json: false,
            allow_partial: false,
            yes: true,
        };

        let result = run(args).await;
        assert!(result.is_ok());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_scan_partial_coverage_requires_allow_partial() {
        let temp_dir =
            std::env::temp_dir().join(format!("asur_test_scan_partial_{}", rand::random::<u64>()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        // Create a file > 1MB so it gets skipped
        let large_data = vec![b'A'; 1_048_576 + 10];
        std::fs::write(temp_dir.join("large.txt"), large_data).unwrap();

        // Without allow_partial, it must bail
        let args_strict = ScanArgs {
            path: temp_dir.clone(),
            install_hook: false,
            no_history: true,
            staged: false,
            json: false,
            allow_partial: false,
            yes: true,
        };
        let res_strict = run(args_strict).await;
        assert!(res_strict.is_err());
        assert!(
            res_strict
                .unwrap_err()
                .to_string()
                .contains("Scan incomplete")
        );

        // With allow_partial, it succeeds
        let args_lenient = ScanArgs {
            path: temp_dir.clone(),
            install_hook: false,
            no_history: true,
            staged: false,
            json: false,
            allow_partial: true,
            yes: true,
        };
        let res_lenient = run(args_lenient).await;
        assert!(res_lenient.is_ok());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_scan_json_partial_coverage_requires_allow_partial() {
        let temp_dir = std::env::temp_dir().join(format!(
            "asur_test_scan_json_partial_{}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let large_data = vec![b'B'; 1_048_576 + 10];
        std::fs::write(temp_dir.join("large2.txt"), large_data).unwrap();

        let args_strict_json = ScanArgs {
            path: temp_dir.clone(),
            install_hook: false,
            no_history: true,
            staged: false,
            json: true,
            allow_partial: false,
            yes: true,
        };
        let res_strict = run(args_strict_json).await;
        assert!(res_strict.is_err());

        let args_lenient_json = ScanArgs {
            path: temp_dir.clone(),
            install_hook: false,
            no_history: true,
            staged: false,
            json: true,
            allow_partial: true,
            yes: true,
        };
        let res_lenient = run(args_lenient_json).await;
        assert!(res_lenient.is_ok());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
