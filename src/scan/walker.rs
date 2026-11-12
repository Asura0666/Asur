use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::allowlist::Allowlist;
use super::rules::{Finding, SecretScanner};

pub const MAX_FILE_SIZE_BYTES: u64 = 1_048_576; // 1 MB

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkippedFile {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct WorkingTreeReport {
    pub findings: Vec<Finding>,
    pub files_scanned: usize,
    pub skipped_files: Vec<SkippedFile>,
    pub directories_visited: usize,
    pub files_discovered: usize,
}

#[derive(Debug, Clone, Default)]
pub struct ScanProgress {
    pub directories_visited: usize,
    pub files_discovered: usize,
    pub files_scanned: usize,
    pub files_skipped: usize,
    pub current_path: Option<String>,
}

/// Scan working tree files starting from `root`.
#[cfg(test)]
pub fn scan_working_tree(
    root: &Path,
    scanner: &SecretScanner,
    allowlist: &Allowlist,
) -> Result<WorkingTreeReport> {
    scan_working_tree_with_progress(root, scanner, allowlist, |_| true)
}

/// Stream traversal and scanning in one pass. `on_progress` can request a
/// cooperative cancellation by returning false. Symlinks are never followed:
/// this prevents a scan from escaping its explicit root.
pub fn scan_working_tree_with_progress<F>(
    root: &Path,
    scanner: &SecretScanner,
    allowlist: &Allowlist,
    mut on_progress: F,
) -> Result<WorkingTreeReport>
where
    F: FnMut(ScanProgress) -> bool,
{
    let mut report = WorkingTreeReport::default();
    scan_directory(
        root,
        root,
        allowlist,
        scanner,
        &mut report,
        &mut on_progress,
    )?;
    Ok(report)
}

fn scan_directory<F>(
    root: &Path,
    current_dir: &Path,
    allowlist: &Allowlist,
    scanner: &SecretScanner,
    report: &mut WorkingTreeReport,
    on_progress: &mut F,
) -> Result<()>
where
    F: FnMut(ScanProgress) -> bool,
{
    report.directories_visited += 1;
    let entries = match std::fs::read_dir(current_dir) {
        Ok(e) => e,
        Err(err) => {
            let rel = current_dir.strip_prefix(root).unwrap_or(current_dir);
            report.skipped_files.push(SkippedFile {
                path: rel.to_string_lossy().replace('\\', "/"),
                reason: format!("directory unreadable: {err}"),
            });
            return Ok(());
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(err) => {
                report.skipped_files.push(SkippedFile {
                    path: path.to_string_lossy().replace('\\', "/"),
                    reason: format!("failed to read file type: {err}"),
                });
                continue;
            }
        };
        if file_type.is_symlink() {
            report.skipped_files.push(SkippedFile {
                path: path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/"),
                reason: "symlink not followed (scan root boundary)".into(),
            });
            continue;
        }
        let file_name = path
            .file_name()
            .map(|s| s.to_string_lossy())
            .unwrap_or_default();

        if should_skip_dir_or_file(&file_name) {
            continue;
        }

        // Relative path for allowlist check
        let rel_path = path.strip_prefix(root).unwrap_or(&path);
        if allowlist.is_path_ignored(rel_path) {
            continue;
        }

        if file_type.is_dir() {
            scan_directory(root, &path, allowlist, scanner, report, on_progress)?;
        } else if file_type.is_file() {
            report.files_discovered += 1;
            if is_binary_extension(&path) {
                continue;
            }
            match path.metadata() {
                Ok(metadata) => {
                    let len = metadata.len();
                    if len > MAX_FILE_SIZE_BYTES {
                        report.skipped_files.push(SkippedFile {
                            path: rel_path.to_string_lossy().replace('\\', "/"),
                            reason: format!(
                                "exceeds {} MB limit ({:.2} MB)",
                                MAX_FILE_SIZE_BYTES / (1024 * 1024),
                                len as f64 / (1024.0 * 1024.0)
                            ),
                        });
                        continue;
                    }
                }
                Err(err) => {
                    report.skipped_files.push(SkippedFile {
                        path: rel_path.to_string_lossy().replace('\\', "/"),
                        reason: format!("failed to read metadata: {err}"),
                    });
                    continue;
                }
            }
            let progress = ScanProgress {
                directories_visited: report.directories_visited,
                files_discovered: report.files_discovered,
                files_scanned: report.files_scanned,
                files_skipped: report.skipped_files.len(),
                current_path: Some(rel_path.to_string_lossy().replace('\\', "/")),
            };
            if !(*on_progress)(progress) {
                anyhow::bail!("scan cancelled");
            }
            match scan_single_file(&path, root, scanner) {
                Ok(Some(file_findings)) => {
                    report.files_scanned += 1;
                    report.findings.extend(file_findings);
                }
                Ok(None) => {}
                Err(err) => report.skipped_files.push(SkippedFile {
                    path: rel_path.to_string_lossy().replace('\\', "/"),
                    reason: format!("read error: {err}"),
                }),
            }
        }
    }

    Ok(())
}

fn should_skip_dir_or_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    matches!(
        lower.as_str(),
        ".git"
            | "target"
            | "node_modules"
            | ".venv"
            | "venv"
            | "dist"
            | "build"
            | ".next"
            | ".nuxt"
            | ".output"
            | ".cache"
            | ".cargo"
            | "vendor"
    )
}

fn is_binary_extension(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    matches!(
        ext.as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "ico"
            | "webp"
            | "mp3"
            | "mp4"
            | "pdf"
            | "zip"
            | "gz"
            | "tar"
            | "7z"
            | "rar"
            | "iso"
            | "exe"
            | "dll"
            | "so"
            | "dylib"
            | "bin"
            | "class"
            | "jar"
            | "war"
            | "pyc"
            | "wasm"
            | "ttf"
            | "woff"
            | "woff2"
            | "eot"
            | "pdb"
    )
}

/// Check if first bytes contain null bytes (indicating binary file)
fn is_binary_content(path: &Path) -> bool {
    let mut f = match File::open(path) {
        Ok(f) => f,
        Err(_) => return true,
    };
    let mut buffer = [0u8; 4096];
    let bytes_read = match f.read(&mut buffer) {
        Ok(n) => n,
        Err(_) => return true,
    };
    buffer[..bytes_read].contains(&0)
}

fn scan_single_file(
    path: &Path,
    root: &Path,
    scanner: &SecretScanner,
) -> Result<Option<Vec<Finding>>> {
    if is_binary_content(path) {
        return Ok(None);
    }

    let file = File::open(path)?;
    let reader = BufReader::new(file);

    let display_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");

    let mut findings = Vec::new();

    for (idx, line_res) in reader.lines().enumerate() {
        let line = line_res?;
        let line_num = idx + 1;
        let line_findings = scanner.scan_line(&display_path, line_num, &line);
        findings.extend(line_findings);
    }

    Ok(Some(findings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_walker_finds_secret() {
        let temp_dir = std::env::temp_dir().join("asur_test_scan_walker");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        let file_path = temp_dir.join("api_config.py");
        fs::write(
            &file_path,
            "import os\napi_key = 'sk_live_1234567890abcdef12345678'\n",
        )
        .unwrap();

        let scanner = SecretScanner::new();
        let allowlist = Allowlist::new();
        let report = scan_working_tree(&temp_dir, &scanner, &allowlist).unwrap();

        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].rule, "Stripe Secret Key");
        assert_eq!(report.findings[0].line, 2);
        assert_eq!(report.files_scanned, 1);
        assert!(report.skipped_files.is_empty());

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_walker_records_large_file_skip() {
        let temp_dir = std::env::temp_dir().join("asur_test_scan_large_file");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        let large_path = temp_dir.join("large_payload.txt");
        let big_data = vec![b'a'; 1_048_576 + 50];
        fs::write(&large_path, big_data).unwrap();

        let scanner = SecretScanner::new();
        let allowlist = Allowlist::new();
        let report = scan_working_tree(&temp_dir, &scanner, &allowlist).unwrap();

        assert_eq!(report.skipped_files.len(), 1);
        assert_eq!(report.skipped_files[0].path, "large_payload.txt");
        assert!(
            report.skipped_files[0]
                .reason
                .contains("exceeds 1 MB limit")
        );

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn streaming_walker_reports_progress_without_buffering_paths() {
        let temp_dir =
            std::env::temp_dir().join(format!("asur_test_scan_progress_{}", rand::random::<u64>()));
        fs::create_dir_all(temp_dir.join("nested")).unwrap();
        fs::write(temp_dir.join("nested/one.txt"), "safe").unwrap();
        fs::write(temp_dir.join("two.txt"), "safe").unwrap();

        let scanner = SecretScanner::new();
        let allowlist = Allowlist::new();
        let mut progress_updates = 0;
        let report = scan_working_tree_with_progress(&temp_dir, &scanner, &allowlist, |_| {
            progress_updates += 1;
            true
        })
        .unwrap();

        assert_eq!(report.files_discovered, 2);
        assert_eq!(report.files_scanned, 2);
        assert!(report.directories_visited >= 2);
        assert_eq!(progress_updates, 2);
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn streaming_walker_honors_cooperative_cancellation() {
        let temp_dir =
            std::env::temp_dir().join(format!("asur_test_scan_cancel_{}", rand::random::<u64>()));
        fs::create_dir_all(&temp_dir).unwrap();
        fs::write(temp_dir.join("one.txt"), "safe").unwrap();
        let scanner = SecretScanner::new();
        let error =
            scan_working_tree_with_progress(&temp_dir, &scanner, &Allowlist::new(), |_| false)
                .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        let _ = fs::remove_dir_all(&temp_dir);
    }
}
