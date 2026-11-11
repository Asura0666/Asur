use regex::Regex;
use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct Allowlist {
    /// Compiled glob regexes
    patterns: Vec<(String, Regex)>,
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
            if let Ok(re) = compile_glob(trimmed) {
                self.patterns.push((trimmed.to_string(), re));
            }
        }
    }

    /// Check if a given file path is matched by the allowlist
    pub fn is_path_ignored(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy().replace('\\', "/");
        let path_clean = path_str.trim_start_matches("./");

        for (_, re) in &self.patterns {
            if re.is_match(path_clean) {
                return true;
            }
        }

        false
    }

    /// Return all active pattern rules
    #[allow(dead_code)]
    pub fn patterns(&self) -> Vec<&str> {
        self.patterns.iter().map(|(p, _)| p.as_str()).collect()
    }
}

/// Compile a gitignore-style glob pattern into a strict Regex.
fn compile_glob(pattern: &str) -> Result<Regex, regex::Error> {
    let pat = pattern.replace('\\', "/");
    let pat = pat.trim();

    // Trailing slash indicates directory matching
    let is_dir = pat.ends_with('/');
    let pat_clean = pat.trim_end_matches('/');

    let mut regex_str = String::from(if pat_clean.contains('/') {
        "^"
    } else {
        "(?:^|/)"
    });

    let mut chars = pat_clean.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    // If followed by slash e.g. "**/", match zero or more directories
                    if chars.peek() == Some(&'/') {
                        chars.next();
                        regex_str.push_str("(?:.*/)?");
                    } else {
                        regex_str.push_str(".*");
                    }
                } else {
                    regex_str.push_str("[^/]*");
                }
            }
            '?' => regex_str.push_str("[^/]"),
            '.' | '+' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' => {
                regex_str.push('\\');
                regex_str.push(ch);
            }
            _ => regex_str.push(ch),
        }
    }

    if is_dir {
        regex_str.push_str("(?:/.*)?$");
    } else {
        regex_str.push('$');
    }

    Regex::new(&regex_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowlist_matching_globs() {
        let mut allow = Allowlist::new();
        allow.parse_lines("# Comment\n\nfixtures/\n*.test.ts\nsample.env\ntests/**/*.key\n");

        // Directory pattern
        assert!(allow.is_path_ignored(Path::new("fixtures/data.json")));
        assert!(allow.is_path_ignored(Path::new("tests/fixtures/test.env")));

        // Extension wildcard pattern
        assert!(allow.is_path_ignored(Path::new("src/auth.test.ts")));
        assert!(allow.is_path_ignored(Path::new("auth.test.ts")));
        assert!(!allow.is_path_ignored(Path::new("src/auth.ts")));
        assert!(!allow.is_path_ignored(Path::new("src/auth.test.ts.bak")));

        // Exact file pattern
        assert!(allow.is_path_ignored(Path::new("sample.env")));
        assert!(allow.is_path_ignored(Path::new("sub/sample.env")));
        assert!(!allow.is_path_ignored(Path::new("sample.env.prod")));

        // Glob with **
        assert!(allow.is_path_ignored(Path::new("tests/certs/client.key")));
        assert!(!allow.is_path_ignored(Path::new("src/client.key")));
    }

    #[test]
    fn test_no_false_positive_substrings() {
        let mut allow = Allowlist::new();
        allow.parse_lines("secret.txt\n");

        assert!(allow.is_path_ignored(Path::new("secret.txt")));
        assert!(allow.is_path_ignored(Path::new("config/secret.txt")));

        // Unrelated file that happens to contain "secret.txt" in its name should not match
        assert!(!allow.is_path_ignored(Path::new("not_secret.txt")));
        assert!(!allow.is_path_ignored(Path::new("secret.txt.old")));
    }
}
