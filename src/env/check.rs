//! Validate required keys and infer types for .env values.

use super::parser::EnvFile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvType {
    Number,
    Bool,
    Url,
    Email,
    String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub key: String,
    pub expected_type: EnvType,
    pub actual_value: String,
    pub message: String,
}

pub fn infer_expected_type(key: &str) -> EnvType {
    let upper = key.to_uppercase();
    if upper == "PORT"
        || upper.ends_with("_PORT")
        || upper.ends_with("_TIMEOUT")
        || upper.starts_with("MAX_")
        || upper.starts_with("MIN_")
        || upper.ends_with("_COUNT")
        || upper.ends_with("_RETRIES")
    {
        EnvType::Number
    } else if upper == "DEBUG"
        || upper.ends_with("_ENABLED")
        || upper.ends_with("_ACTIVE")
        || upper.ends_with("_SECURE")
        || upper.ends_with("_TLS")
        || upper.ends_with("_SSL")
    {
        EnvType::Bool
    } else if upper.ends_with("_URL") || upper.ends_with("_URI") {
        EnvType::Url
    } else if upper.ends_with("_EMAIL") || upper == "EMAIL" {
        EnvType::Email
    } else {
        EnvType::String
    }
}

pub fn validate_type(expected: EnvType, value: &str) -> bool {
    let val = value.trim();
    if val.is_empty() {
        return false;
    }

    match expected {
        EnvType::Number => val.parse::<i64>().is_ok(),
        EnvType::Bool => {
            let lower = val.to_lowercase();
            lower == "true" || lower == "false" || lower == "1" || lower == "0"
        }
        EnvType::Url => {
            val.starts_with("http://")
                || val.starts_with("https://")
                || val.starts_with("postgres://")
                || val.starts_with("postgresql://")
                || val.starts_with("redis://")
                || val.starts_with("mysql://")
                || val.starts_with("mongodb://")
                || val.starts_with("amqp://")
        }
        EnvType::Email => {
            val.contains('@') && val.contains('.') && !val.starts_with('@') && !val.ends_with('@')
        }
        EnvType::String => true,
    }
}

pub fn validate_env(env: &EnvFile) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    for line in &env.lines {
        if let super::parser::EnvLine::Entry { key, value, .. } = line {
            let expected = infer_expected_type(key);
            if expected != EnvType::String && !validate_type(expected, value) {
                let type_name = match expected {
                    EnvType::Number => "number (e.g. 3000)",
                    EnvType::Bool => "boolean (true/false, 1/0)",
                    EnvType::Url => "valid URL (http://, postgres://, etc.)",
                    EnvType::Email => "valid email address",
                    EnvType::String => "string",
                };
                errors.push(ValidationError {
                    key: key.clone(),
                    expected_type: expected,
                    actual_value: value.clone(),
                    message: format!("expected {type_name}, found `{value}`"),
                });
            }
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_infer_and_validate_types() {
        assert_eq!(infer_expected_type("PORT"), EnvType::Number);
        assert!(validate_type(EnvType::Number, "3000"));
        assert!(!validate_type(EnvType::Number, "three-thousand"));

        assert_eq!(infer_expected_type("DEBUG"), EnvType::Bool);
        assert!(validate_type(EnvType::Bool, "true"));
        assert!(validate_type(EnvType::Bool, "1"));
        assert!(!validate_type(EnvType::Bool, "maybe"));

        assert_eq!(infer_expected_type("DATABASE_URL"), EnvType::Url);
        assert!(validate_type(EnvType::Url, "postgres://localhost:5432/db"));
        assert!(!validate_type(EnvType::Url, "not-a-url"));

        assert_eq!(infer_expected_type("ADMIN_EMAIL"), EnvType::Email);
        assert!(validate_type(EnvType::Email, "admin@example.com"));
        assert!(!validate_type(EnvType::Email, "admin"));
    }

    #[test]
    fn test_validate_env() {
        let content = "
PORT=not_a_number
DATABASE_URL=postgres://localhost/db
DEBUG=invalid_bool
";
        let env = EnvFile::parse(content);
        let errors = validate_env(&env);
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].key, "PORT");
        assert_eq!(errors[1].key, "DEBUG");
    }
}
