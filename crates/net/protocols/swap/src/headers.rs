//! SWAP protocol headers for exchange rate negotiation.
//!
//! SWAP uses the "headler" pattern where exchange rates are negotiated via headers
//! before the actual cheque transfer.

use std::collections::HashMap;

use alloy_primitives::U256;
use bytes::Bytes;

/// Header name for exchange rate (price per chunk in wei).
pub const HEADER_EXCHANGE_RATE: &str = "exchange";

/// Header name for deduction (amount to deduct from payment).
pub const HEADER_DEDUCTION: &str = "deduction";

/// Settlement headers exchanged during SWAP protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementHeaders {
    /// Exchange rate (price per chunk in wei).
    pub exchange_rate: U256,
    /// Deduction amount.
    pub deduction: U256,
}

impl SettlementHeaders {
    /// Create new settlement headers.
    pub fn new(exchange_rate: U256, deduction: U256) -> Self {
        Self {
            exchange_rate,
            deduction,
        }
    }

    /// Create headers with only exchange rate (zero deduction).
    pub fn with_rate(exchange_rate: U256) -> Self {
        Self {
            exchange_rate,
            deduction: U256::ZERO,
        }
    }

    /// Parse settlement headers from a header map.
    pub fn from_headers(headers: &HashMap<String, Bytes>) -> Option<Self> {
        let exchange_rate = headers
            .get(HEADER_EXCHANGE_RATE)
            .and_then(parse_u256_bytes)?;

        let deduction = headers
            .get(HEADER_DEDUCTION)
            .and_then(parse_u256_bytes)
            .unwrap_or(U256::ZERO);

        Some(Self {
            exchange_rate,
            deduction,
        })
    }

    /// Convert to a header map for transmission.
    pub fn to_headers(&self) -> HashMap<String, Bytes> {
        let mut headers = HashMap::new();

        headers.insert(
            HEADER_EXCHANGE_RATE.to_string(),
            encode_u256_bytes(self.exchange_rate),
        );

        if self.deduction != U256::ZERO {
            headers.insert(
                HEADER_DEDUCTION.to_string(),
                encode_u256_bytes(self.deduction),
            );
        }

        headers
    }
}

/// Parse a U256 from big-endian bytes (with trimmed leading zeros).
fn parse_u256_bytes(bytes: &Bytes) -> Option<U256> {
    if bytes.is_empty() {
        return Some(U256::ZERO);
    }
    if bytes.len() > 32 {
        return None;
    }
    Some(U256::from_be_slice(bytes))
}

/// Encode a U256 to big-endian bytes (with trimmed leading zeros).
fn encode_u256_bytes(value: U256) -> Bytes {
    let bytes = value.to_be_bytes::<32>();
    match bytes.iter().position(|&b| b != 0) {
        Some(pos) => Bytes::copy_from_slice(&bytes[pos..]),
        None => Bytes::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_settlement_headers_roundtrip() {
        let original = SettlementHeaders::new(U256::from(1_000_000u64), U256::from(500u64));

        let headers = original.to_headers();
        let decoded = SettlementHeaders::from_headers(&headers).unwrap();

        assert_eq!(original, decoded);
    }

    #[test]
    fn test_settlement_headers_zero_deduction() {
        let original = SettlementHeaders::with_rate(U256::from(1_000_000u64));

        let headers = original.to_headers();
        // Zero deduction should not be present in headers
        assert!(!headers.contains_key(HEADER_DEDUCTION));

        let decoded = SettlementHeaders::from_headers(&headers).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_u256_encoding() {
        // Test small value
        let small = U256::from(256u64);
        let encoded = encode_u256_bytes(small);
        assert_eq!(encoded.as_ref(), &[0x01, 0x00]);
        assert_eq!(parse_u256_bytes(&encoded), Some(small));

        // Test zero
        let zero = U256::ZERO;
        let encoded = encode_u256_bytes(zero);
        assert!(encoded.is_empty());
        assert_eq!(parse_u256_bytes(&encoded), Some(zero));

        // Test large value
        let large = U256::MAX;
        let encoded = encode_u256_bytes(large);
        assert_eq!(encoded.len(), 32);
        assert_eq!(parse_u256_bytes(&encoded), Some(large));
    }
}
