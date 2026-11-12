use regex::Regex;
use serde::{Deserialize, Serialize};

use super::entropy::shannon_entropy;
use super::mask::mask_secret;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Finding {
    pub file: String,
    pub line: usize,
    pub rule: String,
    pub masked: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

#[derive(Clone)]
pub struct SecretScanner {
    patterns: Vec<(&'static str, Regex)>,
    assignment_pattern: Regex,
}

impl SecretScanner {
    pub fn new() -> Self {
        let patterns = vec![
            (
                "AWS Access Key ID",
                Regex::new(r"\b(AKIA[0-9A-Z]{16})\b").expect("valid regex"),
            ),
            (
                "AWS Secret Access Key",
                Regex::new(r#"(?i)\b(?:aws_secret_access_key|aws_secret_key)\s*[:=]\s*["']?([A-Za-z0-9/+=]{40})["']?"#)
                    .expect("valid regex"),
            ),
            (
                "GitHub Token",
                Regex::new(r"\b(ghp_[0-9a-zA-Z]{36}|gho_[0-9a-zA-Z]{36}|ghu_[0-9a-zA-Z]{36}|ghs_[0-9a-zA-Z]{36}|ghr_[0-9a-zA-Z]{36}|github_pat_[0-9a-zA-Z_]{82})\b")
                    .expect("valid regex"),
            ),
            (
                "Stripe Secret Key",
                Regex::new(r"\b((?:sk|rk)_live_[0-9a-zA-Z]{24,})\b").expect("valid regex"),
            ),
            (
                "Google API Key",
                Regex::new(r"\b(AIza[0-9A-Za-z\-_]{35})\b").expect("valid regex"),
            ),
            (
                "Slack Token",
                Regex::new(r"\b(xox[baprs]-[0-9a-zA-Z]{10,48})\b").expect("valid regex"),
            ),
            (
                "Private Key",
                Regex::new(r"-----BEGIN\s+[A-Z0-9_\s-]*KEY-----").expect("valid regex"),
            ),
            (
                "Anthropic API Key",
                Regex::new(r"\b(sk-ant-[a-zA-Z0-9_\-]{32,100})\b").expect("valid regex"),
            ),
            (
                "OpenAI API Key",
                Regex::new(r"\b(sk-[a-zA-Z0-9]{32,64})\b").expect("valid regex"),
            ),
        ];

        let assignment_pattern = Regex::new(
            r#"(?i)["']?\b(password|secret|api_key|apikey|access_token|auth_token|client_secret|private_key)\b["']?\s*[:=]\s*["']([^"'\r\n\t ]{16,})["']"#,
        )
        .expect("valid regex");

        Self {
            patterns,
            assignment_pattern,
        }
    }

    /// Check a single line for secrets.
    /// Returns any findings for that line.
    pub fn scan_line(&self, file: &str, line_num: usize, line: &str) -> Vec<Finding> {
        let mut findings = Vec::new();

        // Check for inline suppression comment
        if line.contains("asur:ignore")
            || line.contains("asur-ignore")
            || line.contains("pragma: allowlist secret")
            || line.contains("gitleaks:allow")
            || line.contains("nosec")
        {
            return findings;
        }

        // 1. Check known provider patterns
        for (name, re) in &self.patterns {
            for cap in re.captures_iter(line) {
                let matched_text = cap
                    .get(1)
                    .map_or(cap.get(0).unwrap().as_str(), |m| m.as_str());
                findings.push(Finding {
                    file: file.to_string(),
                    line: line_num,
                    rule: (*name).to_string(),
                    masked: mask_secret(matched_text),
                    commit: None,
                });
            }
        }

        // If a known provider pattern already matched on this line, don't double-flag as generic
        if !findings.is_empty() {
            return findings;
        }

        // 2. Check generic variable assignment with high entropy
        for cap in self.assignment_pattern.captures_iter(line) {
            if let Some(val_match) = cap.get(2) {
                let value = val_match.as_str();
                if is_suspicious_secret(value) {
                    findings.push(Finding {
                        file: file.to_string(),
                        line: line_num,
                        rule: "High-Entropy Secret Assignment".to_string(),
                        masked: mask_secret(value),
                        commit: None,
                    });
                }
            }
        }

        findings
    }
}

impl Default for SecretScanner {
    fn default() -> Self {
        Self::new()
    }
}

/// Determine whether a generic assigned string looks like an actual secret or just a placeholder.
fn is_suspicious_secret(value: &str) -> bool {
    let lower = value.to_lowercase();

    // Ignore obvious placeholders and common non-secrets
    let placeholders = [
        "your-api-key",
        "your_api_key",
        "placeholder",
        "changeme",
        "change-me",
        "change_me",
        "example",
        "dummy",
        "todo",
        "secret-key-goes-here",
        "localhost",
        "127.0.0.1",
        "****************",
        "xxxxxxxxxxxxxxxx",
    ];

    for ph in &placeholders {
        if lower.contains(ph) {
            return false;
        }
    }

    // Minimum length check
    if value.len() < 16 {
        return false;
    }

    // Check Shannon entropy (random API keys/passwords typically >= 3.6)
    let entropy = shannon_entropy(value);
    entropy >= 3.6
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scan_aws_key() {
        let scanner = SecretScanner::new();
        let findings = scanner.scan_line("config.js", 10, "const key = 'AKIAIOSFODNN7EXAMPLE';");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, "AWS Access Key ID");
        assert_eq!(findings[0].masked, "AKIA****MPLE");
    }

    #[test]
    fn test_scan_github_token() {
        let scanner = SecretScanner::new();
        let findings = scanner.scan_line(
            "env.sh",
            5,
            "export GITHUB_TOKEN=ghp_0123456789abcdef0123456789abcdef0123",
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, "GitHub Token");
        assert_eq!(findings[0].masked, "ghp_****0123");
    }

    #[test]
    fn test_scan_stripe_key() {
        let scanner = SecretScanner::new();
        let findings = scanner.scan_line(
            "billing.py",
            12,
            "stripe.api_key = 'sk_live_51A2B3C4D5E6F7G8H9I0J1K2L3'",
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, "Stripe Secret Key");
    }

    #[test]
    fn test_scan_private_key() {
        let scanner = SecretScanner::new();
        let findings = scanner.scan_line("id_rsa", 1, "-----BEGIN RSA PRIVATE KEY-----");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, "Private Key");
    }

    #[test]
    fn test_scan_inline_ignore() {
        let scanner = SecretScanner::new();
        let findings = scanner.scan_line(
            "test.js",
            1,
            "const key = 'AKIAIOSFODNN7EXAMPLE'; // asur:ignore",
        );
        assert!(findings.is_empty(), "should be suppressed by asur:ignore");
    }

    #[test]
    fn test_scan_generic_high_entropy() {
        let scanner = SecretScanner::new();
        let findings = scanner.scan_line(
            "settings.json",
            4,
            r#""api_key": "9f8a7b6c5d4e3f2a1b0c9d8e7f6a5b4c""#,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, "High-Entropy Secret Assignment");
    }

    #[test]
    fn test_scan_generic_placeholder_ignored() {
        let scanner = SecretScanner::new();
        let findings = scanner.scan_line(
            "settings.json",
            4,
            r#""api_key": "your-api-key-here-please-change""#,
        );
        assert!(findings.is_empty(), "placeholder should not be flagged");
    }
}
