//! Parser for .env files preserving comments, multiline values, and structure.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvLine {
    Comment(String),
    Empty,
    Entry {
        key: String,
        value: String,
        comment: Option<String>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvFile {
    pub lines: Vec<EnvLine>,
}

impl EnvFile {
    pub fn parse(content: &str) -> Self {
        let mut lines = Vec::new();
        let mut line_iter = content.lines().peekable();

        while let Some(raw_line) = line_iter.next() {
            let trimmed = raw_line.trim();

            if trimmed.is_empty() {
                lines.push(EnvLine::Empty);
                continue;
            }

            if trimmed.starts_with('#') {
                lines.push(EnvLine::Comment(raw_line.to_string()));
                continue;
            }

            // Strip optional "export " prefix
            let clean_line = if let Some(stripped) = trimmed.strip_prefix("export ") {
                stripped.trim_start()
            } else {
                trimmed
            };

            if let Some((k, rest)) = clean_line.split_once('=') {
                let key = k.trim().to_string();
                let rest_trimmed = rest.trim_start();

                // Check for multiline quoted value
                let (val, comment) = if is_multiline_start(rest_trimmed) {
                    parse_multiline_value(rest_trimmed, &mut line_iter)
                } else {
                    extract_value_and_comment(rest_trimmed)
                };

                lines.push(EnvLine::Entry {
                    key,
                    value: val,
                    comment,
                });
            } else {
                lines.push(EnvLine::Comment(raw_line.to_string()));
            }
        }

        Self { lines }
    }

    pub fn keys(&self) -> Vec<String> {
        self.lines
            .iter()
            .filter_map(|l| match l {
                EnvLine::Entry { key, .. } => Some(key.clone()),
                _ => None,
            })
            .collect()
    }

    #[allow(dead_code)]
    pub fn get(&self, target_key: &str) -> Option<&str> {
        self.lines.iter().find_map(|l| match l {
            EnvLine::Entry { key, value, .. } if key == target_key => Some(value.as_str()),
            _ => None,
        })
    }
}

/// Check if line begins with a quote that is NOT closed on the same line
fn is_multiline_start(s: &str) -> bool {
    if let Some(quote) = s.chars().next() {
        if quote == '"' || quote == '\'' {
            let remainder = &s[1..];
            let mut escaped = false;
            for ch in remainder.chars() {
                if escaped {
                    escaped = false;
                    continue;
                }
                if ch == '\\' && quote == '"' {
                    escaped = true;
                    continue;
                }
                if ch == quote {
                    return false; // Closed on the same line
                }
            }
            return true; // Reached end of line without closing quote
        }
    }
    false
}

/// Accumulate lines until closing quote is found
fn parse_multiline_value<'a, I>(
    first_line: &str,
    lines: &mut std::iter::Peekable<I>,
) -> (String, Option<String>)
where
    I: Iterator<Item = &'a str>,
{
    let quote = first_line.chars().next().unwrap();
    let mut accumulated = first_line[1..].to_string();
    let mut closed = false;
    let mut comment = None;

    for next_line in lines.by_ref() {
        accumulated.push('\n');

        let mut escaped = false;
        let mut close_idx = None;

        for (i, ch) in next_line.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' && quote == '"' {
                escaped = true;
                continue;
            }
            if ch == quote {
                close_idx = Some(i);
                break;
            }
        }

        if let Some(idx) = close_idx {
            accumulated.push_str(&next_line[..idx]);
            let after = next_line[idx + 1..].trim();
            if let Some(c) = after.strip_prefix('#') {
                comment = Some(format!("#{}", c));
            }
            closed = true;
            break;
        } else {
            accumulated.push_str(next_line);
        }
    }

    if !closed {
        // If unclosed, return as-is
        return (accumulated, None);
    }

    let final_val = if quote == '"' {
        unescape_double_quotes(&accumulated)
    } else {
        accumulated
    };

    (final_val, comment)
}

/// Parse single-line value and inline comment
fn extract_value_and_comment(s: &str) -> (String, Option<String>) {
    let s_trimmed = s.trim();

    // 1. Double quoted: "..."
    if let Some(inner) = s_trimmed.strip_prefix('"') {
        let mut val = String::new();
        let mut chars = inner.char_indices();
        let mut end_pos = None;

        let mut escaped = false;
        for (idx, ch) in chars.by_ref() {
            if escaped {
                match ch {
                    'n' => val.push('\n'),
                    'r' => val.push('\r'),
                    't' => val.push('\t'),
                    '\\' => val.push('\\'),
                    '"' => val.push('"'),
                    other => {
                        val.push('\\');
                        val.push(other);
                    }
                }
                escaped = false;
                continue;
            }

            if ch == '\\' {
                escaped = true;
                continue;
            }

            if ch == '"' {
                end_pos = Some(idx);
                break;
            }

            val.push(ch);
        }

        if let Some(idx) = end_pos {
            let after = inner[idx + 1..].trim();
            let comment = after.strip_prefix('#').map(|c| format!("#{}", c));
            return (val, comment);
        } else {
            return (inner.to_string(), None);
        }
    }

    // 2. Single quoted: '...'
    if let Some(inner) = s_trimmed.strip_prefix('\'') {
        if let Some(idx) = inner.find('\'') {
            let val = inner[..idx].to_string();
            let after = inner[idx + 1..].trim();
            let comment = after.strip_prefix('#').map(|c| format!("#{}", c));
            return (val, comment);
        }
    }

    // 3. Unquoted: check for inline comment preceded by space e.g. "foo # comment"
    if let Some(idx) = find_unquoted_inline_comment(s_trimmed) {
        let val = s_trimmed[..idx].trim().to_string();
        let comment = s_trimmed[idx..].trim().to_string();
        (val, Some(comment))
    } else {
        (s_trimmed.to_string(), None)
    }
}

/// Find index of inline comment in unquoted value (must be preceded by whitespace or at start)
fn find_unquoted_inline_comment(s: &str) -> Option<usize> {
    let mut prev_is_whitespace = false;

    for (idx, ch) in s.char_indices() {
        if ch == '#' && prev_is_whitespace {
            return Some(idx);
        }
        prev_is_whitespace = ch.is_whitespace();
    }

    None
}

fn unescape_double_quotes(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                match next {
                    'n' => result.push('\n'),
                    'r' => result.push('\r'),
                    't' => result.push('\t'),
                    '\\' => result.push('\\'),
                    '"' => result.push('"'),
                    other => {
                        result.push('\\');
                        result.push(other);
                    }
                }
            } else {
                result.push('\\');
            }
        } else {
            result.push(ch);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_env_basic() {
        let content = "
# Database Config
DATABASE_URL=postgres://user:pass@localhost:5432/db
PORT=3000 # inline comment

# Auth
JWT_SECRET=\"my-secret-key\"
DEBUG=true
";
        let env = EnvFile::parse(content);
        assert_eq!(env.get("PORT"), Some("3000"));
        assert_eq!(
            env.get("DATABASE_URL"),
            Some("postgres://user:pass@localhost:5432/db")
        );
        assert_eq!(env.get("JWT_SECRET"), Some("my-secret-key"));
        assert_eq!(env.get("DEBUG"), Some("true"));

        let keys = env.keys();
        assert_eq!(keys, vec!["DATABASE_URL", "PORT", "JWT_SECRET", "DEBUG"]);
    }

    #[test]
    fn test_parse_escaped_quotes() {
        let content = r#"JSON_PAYLOAD="{\"name\": \"Alice\", \"role\": \"admin\"}""#;
        let env = EnvFile::parse(content);
        assert_eq!(
            env.get("JSON_PAYLOAD"),
            Some(r#"{"name": "Alice", "role": "admin"}"#)
        );
    }

    #[test]
    fn test_parse_multiline_value() {
        let content = r#"PRIVATE_KEY="-----BEGIN RSA PRIVATE KEY-----
MIIEowIBAAKCAQEA0
-----END RSA PRIVATE KEY-----" # Key file"#;

        let env = EnvFile::parse(content);
        let val = env.get("PRIVATE_KEY").unwrap();
        assert!(val.contains("BEGIN RSA PRIVATE KEY"));
        assert!(val.contains("MIIEowIBAAKCAQEA0"));
        assert!(val.contains("END RSA PRIVATE KEY"));
    }

    #[test]
    fn test_parse_hash_in_unquoted_url() {
        // # Without preceding space should NOT be treated as a comment
        let content = "APP_URL=https://example.com/docs#introduction";
        let env = EnvFile::parse(content);
        assert_eq!(
            env.get("APP_URL"),
            Some("https://example.com/docs#introduction")
        );
    }

    #[test]
    fn test_parse_hash_inside_quotes() {
        let content = r#"SECRET_PASS="pass#word#123" # comment here"#;
        let env = EnvFile::parse(content);
        assert_eq!(env.get("SECRET_PASS"), Some("pass#word#123"));
    }

    #[test]
    fn test_parse_export_syntax() {
        let content = "export API_KEY=xyz123";
        let env = EnvFile::parse(content);
        assert_eq!(env.get("API_KEY"), Some("xyz123"));
    }
}
