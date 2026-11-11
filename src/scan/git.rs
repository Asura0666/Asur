use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::allowlist::Allowlist;
use super::rules::{Finding, SecretScanner};

/// Check if directory is inside a git repository
pub fn is_git_repo(path: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Scan git commit history for secrets in commits
pub fn scan_git_history(
    repo_root: &Path,
    scanner: &SecretScanner,
    allowlist: &Allowlist,
    max_commits: Option<usize>,
) -> Result<Vec<Finding>> {
    if !is_git_repo(repo_root) {
        return Ok(Vec::new());
    }

    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo_root)
        .arg("log")
        .arg("-p")
        .arg("-U0")
        .arg("--no-color");

    if let Some(max) = max_commits {
        cmd.arg(format!("--max-count={max}"));
    }

    let output = match cmd.output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return Ok(Vec::new()),
    };

    Ok(parse_git_diff_output(&output, scanner, allowlist, false))
}

/// Scan only staged changes (e.g. for pre-commit hook)
pub fn scan_staged_changes(
    repo_root: &Path,
    scanner: &SecretScanner,
    allowlist: &Allowlist,
) -> Result<Vec<Finding>> {
    if !is_git_repo(repo_root) {
        return Ok(Vec::new());
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("diff")
        .arg("--cached")
        .arg("-U0")
        .arg("--no-color")
        .output()
        .context("failed to run git diff --cached")?;

    if !output.status.success() {
        bail!("git diff --cached exited with non-zero status");
    }

    let diff_text = String::from_utf8_lossy(&output.stdout);
    Ok(parse_git_diff_output(&diff_text, scanner, allowlist, true))
}

fn parse_git_diff_output(
    diff: &str,
    scanner: &SecretScanner,
    allowlist: &Allowlist,
    is_staged: bool,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut current_commit = None;
    let mut current_file = String::new();
    let mut current_line_num = 1usize;

    for line in diff.lines() {
        if line.starts_with("commit ") {
            let hash = line.trim_start_matches("commit ").trim();
            let short = if hash.len() >= 8 { &hash[..8] } else { hash };
            current_commit = Some(short.to_string());
            continue;
        }

        if line.starts_with("diff --git ") {
            // e.g. "diff --git a/src/main.rs b/src/main.rs"
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                let path_b = parts[3].trim_start_matches("b/");
                current_file = path_b.to_string();
            }
            continue;
        }

        if line.starts_with("@@ ") {
            // e.g. "@@ -10,0 +15,3 @@"
            if let Some(plus_idx) = line.find('+') {
                let rest = &line[plus_idx + 1..];
                let num_str = rest.split([',', ' ']).next().unwrap_or("1");
                current_line_num = num_str.parse().unwrap_or(1);
            }
            continue;
        }

        if line.starts_with('+') && !line.starts_with("+++") {
            let added_content = &line[1..];

            if !current_file.is_empty() && !allowlist.is_path_ignored(Path::new(&current_file)) {
                let mut line_findings =
                    scanner.scan_line(&current_file, current_line_num, added_content);
                for finding in &mut line_findings {
                    if !is_staged {
                        finding.commit = current_commit.clone();
                    }
                }
                findings.extend(line_findings);
            }
            current_line_num += 1;
        }
    }

    findings
}

/// Find git hooks directory for a repo
fn get_hooks_dir(repo_root: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("rev-parse")
        .arg("--git-path")
        .arg("hooks")
        .output();

    if let Ok(out) = output {
        if out.status.success() {
            let rel_or_abs = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let p = PathBuf::from(rel_or_abs);
            if p.is_absolute() {
                return Ok(p);
            } else {
                return Ok(repo_root.join(p));
            }
        }
    }

    Ok(repo_root.join(".git").join("hooks"))
}

/// Installs a pre-commit hook that runs `asur scan --staged`
pub fn install_pre_commit_hook(repo_root: &Path) -> Result<PathBuf> {
    if !is_git_repo(repo_root) {
        bail!("`{}` is not a git repository", repo_root.display());
    }

    let hooks_dir = get_hooks_dir(repo_root)?;
    std::fs::create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join("pre-commit");
    let hook_snippet = r#"
# --- Asur Secret Scanner Hook ---
asur scan --staged
ASUR_EXIT=$?
if [ $ASUR_EXIT -ne 0 ]; then
    echo "Commit rejected: Asur detected leaked secrets in staged changes."
    echo "Fix or rotate the secret, or add '# asur:ignore' to allowlist it."
    exit 1
fi
# --- End Asur Hook ---
"#;

    if hook_path.exists() {
        let existing = std::fs::read_to_string(&hook_path)?;
        if existing.contains("asur scan") {
            return Ok(hook_path);
        }
        // Append without clobbering existing hook
        let updated = format!("{}\n{}", existing.trim_end(), hook_snippet);
        std::fs::write(&hook_path, updated)?;
    } else {
        // Create new hook
        let new_hook = format!("#!/bin/sh\n{}", hook_snippet);
        std::fs::write(&hook_path, new_hook)?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&hook_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&hook_path, perms)?;
    }

    Ok(hook_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_diff_with_secret() {
        let diff = r#"
commit 3a4b5c6d7e8f90123456
Author: Developer <dev@example.com>
Date:   Wed Nov 11 12:00:00 2026

diff --git a/config.json b/config.json
index 0000000..1111111 100644
--- a/config.json
+++ b/config.json
@@ -0,0 +1,2 @@
+{
+  "aws_key": "AKIA1234567890ABCDEF"
+}
"#;
        let scanner = SecretScanner::new();
        let allowlist = Allowlist::new();
        let findings = parse_git_diff_output(diff, &scanner, &allowlist, false);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "config.json");
        assert_eq!(findings[0].rule, "AWS Access Key ID");
        assert_eq!(findings[0].commit.as_deref(), Some("3a4b5c6d"));
    }
}
