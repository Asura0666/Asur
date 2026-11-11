/// Masks a secret string so it can be safely displayed in the terminal and logs
/// without ever exposing the raw secret value.
/// Example: `AKIA1234567890ABCDEF` -> `AKIA****CDEF`
pub fn mask_secret(secret: &str) -> String {
    let trimmed = secret.trim();

    if trimmed.is_empty() {
        return String::new();
    }

    if trimmed.starts_with("-----BEGIN") {
        return "-----BEGIN...KEY-----".to_string();
    }

    let char_count = trimmed.chars().count();
    if char_count <= 4 {
        return "****".to_string();
    }

    if char_count <= 8 {
        let prefix: String = trimmed.chars().take(2).collect();
        let suffix: String = trimmed.chars().skip(char_count - 2).collect();
        return format!("{prefix}****{suffix}");
    }

    let prefix: String = trimmed.chars().take(4).collect();
    let suffix: String = trimmed.chars().skip(char_count - 4).collect();
    format!("{prefix}****{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_secret_empty() {
        assert_eq!(mask_secret(""), "");
        assert_eq!(mask_secret("   "), "");
    }

    #[test]
    fn test_mask_secret_short() {
        assert_eq!(mask_secret("abcd"), "****");
        assert_eq!(mask_secret("abcdef"), "ab****ef");
    }

    #[test]
    fn test_mask_secret_aws() {
        assert_eq!(
            mask_secret("AKIAIOSFODNN7EXAMPLE"), // asur:ignore
            "AKIA****MPLE"
        );
    }

    #[test]
    fn test_mask_secret_private_key() {
        assert_eq!(
            mask_secret("-----BEGIN RSA PRIVATE KEY-----"), // asur:ignore
            "-----BEGIN...KEY-----"
        );
    }
}
