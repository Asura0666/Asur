//! Compare .env and .env.example files.

use std::collections::HashSet;

use super::parser::EnvFile;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnvDiffReport {
    pub missing_in_env: Vec<String>, // Present in .env.example, missing in .env
    pub extra_in_env: Vec<String>,   // Present in .env, missing in .env.example
    pub matching_keys: Vec<String>,  // Present in both
}

pub fn diff_env(env: &EnvFile, example: &EnvFile) -> EnvDiffReport {
    let env_keys: HashSet<String> = env.keys().into_iter().collect();
    let example_keys: HashSet<String> = example.keys().into_iter().collect();

    let mut missing_in_env: Vec<String> = example_keys
        .iter()
        .filter(|k| !env_keys.contains(*k))
        .cloned()
        .collect();
    missing_in_env.sort();

    let mut extra_in_env: Vec<String> = env_keys
        .iter()
        .filter(|k| !example_keys.contains(*k))
        .cloned()
        .collect();
    extra_in_env.sort();

    let mut matching_keys: Vec<String> = env_keys
        .iter()
        .filter(|k| example_keys.contains(*k))
        .cloned()
        .collect();
    matching_keys.sort();

    EnvDiffReport {
        missing_in_env,
        extra_in_env,
        matching_keys,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_env() {
        let env = EnvFile::parse("A=1\nB=2\nEXTRA=3\n");
        let example = EnvFile::parse("A=\nB=\nMISSING=\n");

        let report = diff_env(&env, &example);
        assert_eq!(report.matching_keys, vec!["A", "B"]);
        assert_eq!(report.missing_in_env, vec!["MISSING"]);
        assert_eq!(report.extra_in_env, vec!["EXTRA"]);
    }
}

