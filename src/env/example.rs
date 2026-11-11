//! Generator for .env.example files from .env.

use super::parser::{EnvFile, EnvLine};

pub fn generate_example(env_file: &EnvFile) -> String {
    let mut out = String::new();

    for line in &env_file.lines {
        match line {
            EnvLine::Comment(c) => {
                out.push_str(c);
                out.push('\n');
            }
            EnvLine::Empty => {
                out.push('\n');
            }
            EnvLine::Entry {
                key,
                value,
                comment,
            } => {
                let placeholder = if is_safe_placeholder(key) {
                    value.clone()
                } else {
                    String::new()
                };

                if let Some(c) = comment {
                    out.push_str(&format!("{key}={placeholder}  {c}\n"));
                } else {
                    out.push_str(&format!("{key}={placeholder}\n"));
                }
            }
        }
    }

    out
}

pub fn is_safe_placeholder(key: &str) -> bool {
    let upper = key.to_uppercase();
    upper == "PORT"
        || upper == "HOST"
        || upper == "NODE_ENV"
        || upper == "APP_ENV"
        || upper == "ENV"
        || upper == "DEBUG"
        || upper == "LOG_LEVEL"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_example_strips_secrets_keeps_safe() {
        let content = "
# Server Settings
PORT=8080
SECRET_KEY=super-secret-pass
DATABASE_URL=postgres://root:pass@localhost/db # db connection
";
        let env = EnvFile::parse(content);
        let example = generate_example(&env);

        assert!(example.contains("PORT=8080"));
        assert!(example.contains("SECRET_KEY=\n"));
        assert!(example.contains("DATABASE_URL=  # db connection"));
        assert!(!example.contains("super-secret-pass"));
        assert!(!example.contains("pass@localhost"));
    }
}

