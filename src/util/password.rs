//! Password generation using cryptographically secure RNG.

use rand::{Rng, rngs::OsRng};

/// Generate a cryptographically strong, random password.
///
/// Uses [A-Za-z0-9] without ambiguous characters (`0, O, l, 1, I`) or
/// URL special characters (`@, :, /, ?, #, &, %`) so the password is
/// immediately safe to use inside connection strings without percent-encoding.
pub fn generate_password(length: usize) -> String {
    const CHARSET: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = OsRng;
    (0..length)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_requested_length() {
        let pass = generate_password(16);
        assert_eq!(pass.len(), 16);
        let pass24 = generate_password(24);
        assert_eq!(pass24.len(), 24);
    }

    #[test]
    fn generates_different_passwords() {
        let p1 = generate_password(20);
        let p2 = generate_password(20);
        assert_ne!(p1, p2);
    }

    #[test]
    fn contains_only_url_safe_chars() {
        let pass = generate_password(100);
        for c in pass.chars() {
            assert!(c.is_ascii_alphanumeric());
            assert!(!['0', 'O', 'l', '1', 'I'].contains(&c));
        }
    }
}
