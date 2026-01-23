//! Identity configuration for TOML persistence.
//!
//! The identity configuration holds the nonce used for overlay address derivation.
//! The signing key itself is stored in a standard Ethereum keystore (JSON v3 format),
//! managed by alloy.

use alloy_primitives::B256;
use rand::RngCore;
use serde::{Deserialize, Serialize};

/// Identity configuration.
///
/// Contains the nonce used for overlay address derivation. The signing key
/// is stored separately in an Ethereum keystore file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityConfig {
    /// Nonce for overlay address derivation (hex-encoded).
    ///
    /// The overlay address is derived as: `keccak256(eth_address || network_id || nonce)`
    ///
    /// Changing the nonce changes the overlay address, which changes the node's
    /// position in the Kademlia DHT and its storage responsibilities.
    ///
    /// If not set, a random nonce is generated on first run.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_nonce",
        deserialize_with = "deserialize_nonce"
    )]
    pub nonce: Option<B256>,

    /// Path to the Ethereum keystore file.
    ///
    /// If not set, defaults to `<datadir>/keys/swarm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keystore_path: Option<String>,
}

impl Default for IdentityConfig {
    fn default() -> Self {
        Self {
            nonce: None,
            keystore_path: None,
        }
    }
}

impl IdentityConfig {
    /// Get the nonce, generating a random one if not set.
    ///
    /// Returns (nonce, was_generated) tuple.
    pub fn nonce_or_generate(&mut self) -> (B256, bool) {
        if let Some(nonce) = self.nonce {
            (nonce, false)
        } else {
            let nonce = generate_random_nonce();
            self.nonce = Some(nonce);
            (nonce, true)
        }
    }
}

/// Generate a random 32-byte nonce.
pub fn generate_random_nonce() -> B256 {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    B256::from(bytes)
}

fn serialize_nonce<S>(nonce: &Option<B256>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match nonce {
        Some(n) => serializer.serialize_str(&format!("0x{}", hex::encode(n.as_slice()))),
        None => serializer.serialize_none(),
    }
}

fn deserialize_nonce<'de, D>(deserializer: D) -> Result<Option<B256>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    let opt: Option<String> = Option::deserialize(deserializer)?;
    match opt {
        Some(s) => {
            let s = s.strip_prefix("0x").unwrap_or(&s);
            let bytes =
                hex::decode(s).map_err(|e| D::Error::custom(format!("invalid hex: {}", e)))?;
            if bytes.len() != 32 {
                return Err(D::Error::custom(format!(
                    "nonce must be 32 bytes, got {}",
                    bytes.len()
                )));
            }
            Ok(Some(B256::from_slice(&bytes)))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_deserialize_nonce() {
        let config = IdentityConfig {
            nonce: Some(B256::from([0x42u8; 32])),
            keystore_path: None,
        };

        let toml = toml::to_string(&config).unwrap();
        assert!(toml.contains("0x4242424242424242424242424242424242424242424242424242424242424242"));

        let parsed: IdentityConfig = toml::from_str(&toml).unwrap();
        assert_eq!(parsed.nonce, config.nonce);
    }

    #[test]
    fn test_nonce_or_generate() {
        let mut config = IdentityConfig::default();
        assert!(config.nonce.is_none());

        let (nonce1, generated1) = config.nonce_or_generate();
        assert!(generated1);
        assert!(config.nonce.is_some());

        let (nonce2, generated2) = config.nonce_or_generate();
        assert!(!generated2);
        assert_eq!(nonce1, nonce2);
    }

    #[test]
    fn test_default_has_no_nonce() {
        let config = IdentityConfig::default();
        let toml = toml::to_string(&config).unwrap();
        // No nonce field when None
        assert!(!toml.contains("nonce"));
    }
}
