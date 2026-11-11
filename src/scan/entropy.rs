use std::collections::HashMap;

/// Calculate the Shannon entropy of a string in bits per character.
/// Range is 0.0 (all characters identical) up to log2(charset_size) (~6.0+ for random base64/hex).
pub fn shannon_entropy(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }

    let mut counts = HashMap::new();
    let mut total_chars = 0usize;

    for ch in s.chars() {
        *counts.entry(ch).or_insert(0usize) += 1;
        total_chars += 1;
    }

    if total_chars <= 1 || counts.len() <= 1 {
        return 0.0;
    }

    let total_f = total_chars as f64;
    let mut entropy = 0.0;

    for &count in counts.values() {
        let p = (count as f64) / total_f;
        entropy -= p * p.log2();
    }

    entropy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shannon_entropy_empty_or_single() {
        assert_eq!(shannon_entropy(""), 0.0);
        assert_eq!(shannon_entropy("a"), 0.0);
        assert_eq!(shannon_entropy("aaaaaaa"), 0.0);
    }

    #[test]
    fn test_shannon_entropy_natural_text() {
        // Natural English text typically has entropy between 2.5 and 3.5
        let text = "this is a normal english sentence without high randomness";
        let ent = shannon_entropy(text);
        assert!(ent > 2.0 && ent < 4.0, "entropy was {ent}");
    }

    #[test]
    fn test_shannon_entropy_random_secret() {
        // High entropy random string (base64 / random characters)
        let secret = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        let ent = shannon_entropy(secret);
        assert!(ent > 4.2, "expected > 4.2, got {ent}");
    }
}

