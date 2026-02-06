//! Spec parser for CLI integration.

use crate::{Spec, init_dev, init_mainnet, init_testnet};
use alloc::sync::Arc;
use nectar_swarms::NamedSwarm;
use std::path::Path;
use strum::VariantNames;
use vertex_swarm_api::SwarmSpecParser;

#[cfg(feature = "clap")]
use clap::builder::{StringValueParser, TypedValueParser};

/// Default spec parser supporting mainnet, testnet, dev, and file-based specs.
#[derive(Clone, Debug, Default)]
pub struct DefaultSpecParser;

impl SwarmSpecParser for DefaultSpecParser {
    type Spec = Spec;

    const SUPPORTED_NETWORKS: &'static [&'static str] = NamedSwarm::VARIANTS;

    fn parse(s: &str) -> eyre::Result<Arc<Self::Spec>> {
        // Try parsing as a named network first
        if let Ok(named) = s.parse::<NamedSwarm>() {
            return Ok(match named {
                NamedSwarm::Mainnet => init_mainnet(),
                NamedSwarm::Testnet => init_testnet(),
                NamedSwarm::Dev => init_dev(),
                _ => return Err(eyre::eyre!("unsupported network: {}", s)),
            });
        }

        // Try as file path
        let path = Path::new(s);
        if path.exists() {
            return Spec::from_file(path)
                .map(Arc::new)
                .map_err(|e| eyre::eyre!("failed to load spec from {}: {}", path.display(), e));
        }

        // Try as inline JSON
        Spec::from_json(s)
            .map(Arc::new)
            .map_err(|e| eyre::eyre!("'{}' is not a valid network, file path, or JSON: {}", s, e))
    }
}

#[cfg(feature = "clap")]
impl DefaultSpecParser {
    /// Clap value parser for CLI integration.
    ///
    /// Accepts "mainnet", "testnet", "dev", a file path, or inline JSON.
    pub fn parser() -> impl TypedValueParser<Value = Arc<Spec>> {
        StringValueParser::new().try_map(|s| Self::parse(&s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_api::SwarmSpec;

    #[test]
    fn test_parse_mainnet() {
        let spec = DefaultSpecParser::parse("mainnet").unwrap();
        assert!(spec.is_mainnet());
    }

    #[test]
    fn test_parse_testnet() {
        let spec = DefaultSpecParser::parse("testnet").unwrap();
        assert!(spec.is_testnet());
    }

    #[test]
    fn test_parse_dev() {
        let spec = DefaultSpecParser::parse("dev").unwrap();
        assert!(spec.is_dev());
    }

    #[test]
    fn test_parse_invalid() {
        let result = DefaultSpecParser::parse("nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_json() {
        let json = r#"{"network_id": 999, "network_name": "test"}"#;
        let spec = DefaultSpecParser::parse(json).unwrap();
        assert_eq!(spec.network_id, 999);
        assert_eq!(spec.network_name, "test");
    }

    #[test]
    fn test_supported_networks() {
        assert!(DefaultSpecParser::SUPPORTED_NETWORKS.contains(&"mainnet"));
        assert!(DefaultSpecParser::SUPPORTED_NETWORKS.contains(&"testnet"));
        assert!(DefaultSpecParser::SUPPORTED_NETWORKS.contains(&"dev"));
    }
}
