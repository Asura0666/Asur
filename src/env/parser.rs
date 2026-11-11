//! Parser for .env files preserving comments and structure.

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

        for raw_line in content.lines() {
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
                let (val_part, inline_comment) = extract_value_and_comment(rest.trim());
                lines.push(EnvLine::Entry {
                    key,
                    value: val_part,
                    comment: inline_comment,
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

fn extract_value_and_comment(s: &str) -> (String, Option<String>) {
    if let Some(inner) = s.strip_prefix('"') {
        if let Some(end_idx) = inner.find('"') {
            let val = &inner[..end_idx];
            let after = inner[end_idx + 1..].trim();
            let comment = after.strip_prefix('#').map(|c| format!("#{}", c));
            return (val.to_string(), comment);
        }
    } else if let Some(inner) = s.strip_prefix('\'') {
        if let Some(end_idx) = inner.find('\'') {
            let val = &inner[..end_idx];
            let after = inner[end_idx + 1..].trim();
            let comment = after.strip_prefix('#').map(|c| format!("#{}", c));
            return (val.to_string(), comment);
        }
    }

    if let Some((v, c)) = s.split_once('#') {
        (v.trim().to_string(), Some(format!("#{}", c)))
    } else {
        (s.to_string(), None)
    }
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
    fn test_parse_export_syntax() {
        let content = "export API_KEY=xyz123";
        let env = EnvFile::parse(content);
        assert_eq!(env.get("API_KEY"), Some("xyz123"));
    }
}
