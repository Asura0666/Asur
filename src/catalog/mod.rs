//! Data-driven catalog of services Asur can start.

use anyhow::{Context, Result};
use serde::Deserialize;

const SERVICES_TOML: &str = include_str!("services.toml");

fn default_tag() -> String {
    "latest".to_string()
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Service {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub image: String,
    #[serde(default = "default_tag")]
    pub default_tag: String,
    pub default_port: u16,
    pub data_path: Option<String>,
    pub connection: String,
    #[allow(dead_code)]
    pub client: Vec<String>,
    pub user_env: Option<String>,
    pub password_env: Option<String>,
    pub database_env: Option<String>,
    pub default_user: Option<String>,
    pub default_database: Option<String>,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub extra_env: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Catalog {
    #[serde(rename = "service")]
    services: Vec<Service>,
}

impl Catalog {
    pub fn load() -> Result<Self> {
        toml::from_str(SERVICES_TOML).context("the built-in service catalog is invalid")
    }

    pub fn services(&self) -> &[Service] {
        &self.services
    }

    /// Exact lookup by name or alias, case-insensitive.
    pub fn find(&self, query: &str) -> Option<&Service> {
        let q = query.trim().to_lowercase();
        self.services
            .iter()
            .find(|s| s.name == q || s.aliases.iter().any(|a| a.to_lowercase() == q))
    }

    /// Find the closest matching service using Jaro-Winkler string similarity.
    /// Returns the best matching service if its similarity score is >= 0.75.
    pub fn suggest(&self, query: &str) -> Option<&Service> {
        let q = query.trim().to_lowercase();
        let mut best: Option<(&Service, f64)> = None;

        for s in &self.services {
            let mut max_sim = strsim::jaro_winkler(&s.name, &q);
            for alias in &s.aliases {
                let sim = strsim::jaro_winkler(&alias.to_lowercase(), &q);
                if sim > max_sim {
                    max_sim = sim;
                }
            }

            if max_sim >= 0.75 {
                if let Some((_, best_score)) = best {
                    if max_sim > best_score {
                        best = Some((s, max_sim));
                    }
                } else {
                    best = Some((s, max_sim));
                }
            }
        }

        best.map(|(s, _)| s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_catalog_parses() {
        let catalog = Catalog::load().expect("catalog should parse");
        assert!(!catalog.services().is_empty());
    }

    #[test]
    fn finds_by_name_and_alias_case_insensitively() {
        let catalog = Catalog::load().unwrap();
        assert_eq!(catalog.find("postgres").unwrap().name, "postgres");
        assert_eq!(catalog.find("PG").unwrap().name, "postgres");
        assert_eq!(catalog.find(" Mongo ").unwrap().name, "mongodb");
        assert_eq!(catalog.find("s3").unwrap().name, "minio");
    }

    #[test]
    fn unknown_service_is_none() {
        let catalog = Catalog::load().unwrap();
        assert!(catalog.find("reids").is_none());
    }

    #[test]
    fn fuzzy_suggest_catches_typos() {
        let catalog = Catalog::load().unwrap();
        assert_eq!(catalog.suggest("reids").unwrap().name, "redis");
        assert_eq!(catalog.suggest("postgre").unwrap().name, "postgres");
        assert_eq!(catalog.suggest("mysq").unwrap().name, "mysql");
        assert_eq!(catalog.suggest("rabit").unwrap().name, "rabbitmq");
        assert_eq!(catalog.suggest("totally_unknown_12345"), None);
    }

    #[test]
    fn every_connection_template_uses_known_placeholders() {
        let catalog = Catalog::load().unwrap();
        for s in catalog.services() {
            let mut rest = s.connection.as_str();
            while let Some(start) = rest.find('{') {
                let end = rest[start..].find('}').expect("unclosed placeholder") + start;
                let key = &rest[start + 1..end];
                assert!(
                    ["user", "password", "port", "database"].contains(&key),
                    "{}: unknown placeholder {{{key}}}",
                    s.name
                );
                rest = &rest[end + 1..];
            }
        }
    }
}
