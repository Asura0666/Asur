use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct Allowlist {
    /// Paths or glob-like substrings to ignore
    ignored_patterns: Vec<String>,
}

impl Allowlist {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load allowlist from a file (e.g. `.asurignore`)
    pub fn from_file<P: AsRef<Path>>(path: P) -> Self {
        let mut allowlist = Self::new();
        if let Ok(content) = std::fs::read_to_string(path) {
            allowlist.parse_lines(&content);
        }
        allowlist
    }

    /// Parse lines of patterns
    pub fn parse_lines(&mut self, content: &str) {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            self.ignored_patterns.push(trimmed.to_string());
        }
    }

    /// Check if a given file path is matched by the allowlist
    pub fn is_path_ignored(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy().replace('\\', "/");

        for pattern in &self.ignored_patterns {
            let pat = pattern.replace('\\', "/");
            let pat = pat.trim_matches('*');
            if pat.is_empty() {
                continue;
            }
            if path_str.contains(pat) {
                return true;
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowlist_matching() {
        let mut allow = Allowlist::new();
        allow.parse_lines("# Comment\n\nfixtures/\n*.test.ts\nsample.env\n");

        assert!(allow.is_path_ignored(Path::new("tests/fixtures/test.env")));
        assert!(allow.is_path_ignored(Path::new("src/auth.test.ts")));
        assert!(allow.is_path_ignored(Path::new("sample.env")));
        assert!(!allow.is_path_ignored(Path::new("src/auth.ts")));
    }
}
