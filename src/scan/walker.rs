use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::allowlist::Allowlist;
use super::rules::{Finding, SecretScanner};

const MAX_FILE_SIZE_BYTES: u64 = 1_048_576; // 1 MB

/// Scan working tree files starting from `root`.
pub fn scan_working_tree(
    root: &Path,
    scanner: &SecretScanner,
    allowlist: &Allowlist,
) -> Result<Vec<Finding>> {
    let mut findings = Vec::new();
    let mut files_to_scan = Vec::new();

    collect_files(root, root, allowlist, &mut files_to_scan)?;

    for file_path in files_to_scan {
        if let Ok(file_findings) = scan_single_file(&file_path, root, scanner) {
            findings.extend(file_findings);
        }
    }

    Ok(findings)
}

fn collect_files(
    root: &Path,
    current_dir: &Path,
    allowlist: &Allowlist,
    collected: &mut Vec<PathBuf>,
) -> Result<()> {
    let entries = match std::fs::read_dir(current_dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
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

        if path.is_dir() {
            collect_files(root, &path, allowlist, collected)?;
        } else if path.is_file() {
            if is_binary_extension(&path) {
                continue;
            }
            if let Ok(metadata) = path.metadata() {
                if metadata.len() > MAX_FILE_SIZE_BYTES {
                    continue;
                }
            }
            collected.push(path);
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

fn scan_single_file(path: &Path, root: &Path, scanner: &SecretScanner) -> Result<Vec<Finding>> {
    if is_binary_content(path) {
        return Ok(Vec::new());
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
        let line = match line_res {
            Ok(l) => l,
            Err(_) => break, // Invalid UTF-8, stop scanning this file
        };
        let line_num = idx + 1;
        let line_findings = scanner.scan_line(&display_path, line_num, &line);
        findings.extend(line_findings);
    }

    Ok(findings)
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
        let findings = scan_working_tree(&temp_dir, &scanner, &allowlist).unwrap();

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, "Stripe Secret Key");
        assert_eq!(findings[0].line, 2);

        let _ = fs::remove_dir_all(&temp_dir);
    }
}

