use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Args;
use owo_colors::{OwoColorize, Stream, Style};

use crate::ui;

#[derive(Args)]
pub struct IgnoreArgs {
    /// Directory to inspect
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// Apply changes without asking
    #[arg(short, long)]
    pub yes: bool,
    /// Show proposed changes without modifying files
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProjectType {
    Rust,
    Node,
    Python,
    Go,
    Java,
}

impl ProjectType {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::Node => "Node.js",
            Self::Python => "Python",
            Self::Go => "Go",
            Self::Java => "Java",
        }
    }
}

pub async fn run(args: IgnoreArgs) -> Result<()> {
    let dir = if args.path.is_relative() {
        std::env::current_dir()?.join(&args.path)
    } else {
        args.path.clone()
    };

    let detected_types = detect_project_types(&dir);
    let has_docker = detect_docker(&dir);

    let mut detected_names: Vec<&str> = detected_types.iter().map(|t| t.name()).collect();
    if has_docker {
        detected_names.push("Docker");
    }

    if detected_names.is_empty() {
        ui::info("No specific project marker found. Applying general local-dev & .env rules.");
    } else {
        ui::info(&format!(
            "Detected project type(s): {}",
            detected_names.join(", ")
        ));
    }

    // 1. Process .gitignore
    let gitignore_rules = collect_gitignore_rules(&detected_types);
    process_ignore_file(
        &dir.join(".gitignore"),
        ".gitignore",
        &gitignore_rules,
        args.yes,
        args.dry_run,
    )?;

    // 2. Process .dockerignore if Docker is detected
    if has_docker {
        let dockerignore_rules = collect_dockerignore_rules(&detected_types);
        process_ignore_file(
            &dir.join(".dockerignore"),
            ".dockerignore",
            &dockerignore_rules,
            args.yes,
            args.dry_run,
        )?;
    }

    Ok(())
}

pub fn detect_project_types(dir: &Path) -> Vec<ProjectType> {
    let mut types = Vec::new();

    if dir.join("Cargo.toml").exists() {
        types.push(ProjectType::Rust);
    }
    if dir.join("package.json").exists() {
        types.push(ProjectType::Node);
    }
    if dir.join("requirements.txt").exists()
        || dir.join("pyproject.toml").exists()
        || dir.join("setup.py").exists()
        || dir.join("Pipfile").exists()
    {
        types.push(ProjectType::Python);
    }
    if dir.join("go.mod").exists() {
        types.push(ProjectType::Go);
    }
    if dir.join("pom.xml").exists()
        || dir.join("build.gradle").exists()
        || dir.join("build.gradle.kts").exists()
    {
        types.push(ProjectType::Java);
    }

    types
}

pub fn detect_docker(dir: &Path) -> bool {
    dir.join("Dockerfile").exists()
        || dir.join("compose.yaml").exists()
        || dir.join("compose.yml").exists()
        || dir.join("docker-compose.yml").exists()
        || dir.join("docker-compose.yaml").exists()
}

pub fn collect_gitignore_rules(types: &[ProjectType]) -> Vec<&'static str> {
    let mut rules = Vec::new();

    // Universal local dev & environment ignores
    rules.extend([
        ".env",
        ".env*.local",
        ".env.production",
        ".env.staging",
        ".DS_Store",
        "Thumbs.db",
    ]);

    for t in types {
        match t {
            ProjectType::Rust => {
                rules.extend(["target/", "**/*.rs.bk", "*.pdb"]);
            }
            ProjectType::Node => {
                rules.extend([
                    "node_modules/",
                    "dist/",
                    "build/",
                    ".next/",
                    ".nuxt/",
                    ".cache/",
                    "*.tsbuildinfo",
                    "npm-debug.log*",
                    "yarn-debug.log*",
                    "yarn-error.log*",
                    ".pnpm-debug.log*",
                ]);
            }
            ProjectType::Python => {
                rules.extend([
                    "__pycache__/",
                    "*.py[cod]",
                    "*$py.class",
                    ".venv/",
                    "venv/",
                    "env/",
                    "*.egg-info/",
                    ".pytest_cache/",
                    ".mypy_cache/",
                ]);
            }
            ProjectType::Go => {
                rules.extend(["bin/", "vendor/"]);
            }
            ProjectType::Java => {
                rules.extend(["target/", "build/", ".gradle/", "*.class"]);
            }
        }
    }

    rules
}

pub fn collect_dockerignore_rules(types: &[ProjectType]) -> Vec<&'static str> {
    let mut rules = vec![
        ".git",
        ".gitignore",
        ".env*",
        "*.md",
        "Dockerfile*",
        "compose*.yml",
        "docker-compose*.yml",
    ];

    for t in types {
        match t {
            ProjectType::Rust => rules.extend(["target"]),
            ProjectType::Node => rules.extend(["node_modules", "dist", ".next", ".nuxt"]),
            ProjectType::Python => rules.extend(["__pycache__", ".venv", "venv"]),
            ProjectType::Go => rules.extend(["vendor", "bin"]),
            ProjectType::Java => rules.extend(["target", "build", ".gradle"]),
        }
    }

    rules
}

/// Filter rules to only those not already covered by existing file contents
pub fn calculate_missing_rules<'a>(
    existing_content: &str,
    suggested_rules: &[&'a str],
) -> Vec<&'a str> {
    let existing_lines: Vec<&str> = existing_content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();

    suggested_rules
        .iter()
        .copied()
        .filter(|rule| {
            let rule_clean = rule.trim_end_matches('/');
            !existing_lines.iter().any(|existing| {
                let existing_clean = existing.trim_end_matches('/');
                existing_clean == rule_clean || *existing == *rule
            })
        })
        .collect()
}

fn process_ignore_file(
    target_path: &Path,
    file_name: &str,
    rules: &[&str],
    auto_yes: bool,
    dry_run: bool,
) -> Result<()> {
    let existing_content = std::fs::read_to_string(target_path).unwrap_or_default();
    let missing_rules = calculate_missing_rules(&existing_content, rules);

    let ok_style = Style::new().green().bold();

    if missing_rules.is_empty() {
        println!(
            "{} `{file_name}` is already up to date.",
            "✔".if_supports_color(Stream::Stdout, |t| t.style(ok_style))
        );
        return Ok(());
    }

    println!("\nProposed additions to `{file_name}`:");
    for rule in &missing_rules {
        let add_style = Style::new().green();
        println!(
            "  {} {rule}",
            "+".if_supports_color(Stream::Stdout, |t| t.style(add_style))
        );
    }
    println!();

    if dry_run {
        ui::info(&format!(
            "[dry-run] Would add {} entries to `{file_name}`.",
            missing_rules.len()
        ));
        return Ok(());
    }

    let should_apply = if auto_yes {
        true
    } else {
        inquire::Confirm::new(&format!("Update `{file_name}`?"))
            .with_default(true)
            .prompt()
            .unwrap_or(false)
    };

    if should_apply {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(target_path)?;

        if !existing_content.is_empty() && !existing_content.ends_with('\n') {
            writeln!(file)?;
        }
        writeln!(file, "\n# Added by asur ignore")?;
        for rule in &missing_rules {
            writeln!(file, "{rule}")?;
        }

        println!(
            "{} Updated `{file_name}` (+{} rule(s)).",
            "✔".if_supports_color(Stream::Stdout, |t| t.style(ok_style)),
            missing_rules.len()
        );
    } else {
        println!("Skipped `{file_name}`.");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_missing_rules() {
        let existing = "# Gitignore\ntarget/\n.env\n";
        let suggested = vec!["target/", ".env", ".env*.local", "**/*.rs.bk"];
        let missing = calculate_missing_rules(existing, &suggested);

        assert_eq!(missing, vec![".env*.local", "**/*.rs.bk"]);
    }

    #[test]
    fn test_collect_gitignore_rules_rust() {
        let types = vec![ProjectType::Rust];
        let rules = collect_gitignore_rules(&types);

        assert!(rules.contains(&".env"));
        assert!(rules.contains(&"target/"));
        assert!(rules.contains(&"**/*.rs.bk"));
    }
}
