//! Cheque types for SWAP settlement.
//!
//! A cheque is a signed commitment to pay a certain cumulative amount from a
//! chequebook contract to a beneficiary. Cheques are exchanged off-chain and
//! can be cashed on-chain at any time.
//!
//! # EIP-712 Signing
//!
//! Cheques use EIP-712 typed data signing with the following domain:
//! - Name: "Chequebook"
//! - Version: "1.0"
//! - ChainId: from SwarmSpec's chain
//!
//! The cheque type is:
//! ```text
//! Cheque(address chequebook,address beneficiary,uint256 cumulativePayout)
//! ```

use alloy_primitives::{Address, B256, Signature, U256, hex};
use alloy_sol_types::{Eip712Domain, SolStruct, eip712_domain};
use bytes::Bytes;
use vertex_swarm_spec::SwarmSpec;

use crate::ChequeError;

// Re-export the Cheque type from nectar-contracts.
pub use nectar_contracts::Cheque;

/// EIP-712 domain name for chequebook signing.
pub const DOMAIN_NAME: &str = "Chequebook";

/// EIP-712 domain version for chequebook signing.
pub const DOMAIN_VERSION: &str = "1.0";

/// Extension trait for `Cheque` providing EIP-712 signing support.
pub trait ChequeExt {
    /// Create a new cheque.
    fn new(chequebook: Address, beneficiary: Address, cumulative_payout: U256) -> Self;

    /// Get the cumulative payout amount.
    fn cumulative_payout(&self) -> U256;

    /// Build the EIP-712 domain for cheque signing.
    ///
    /// The chain ID is derived from the SwarmSpec's underlying chain.
    fn domain(spec: &impl SwarmSpec) -> Eip712Domain;

    /// Compute the EIP-712 signing hash for this cheque.
    fn signing_hash(&self, spec: &impl SwarmSpec) -> B256;
}

impl ChequeExt for Cheque {
    fn new(chequebook: Address, beneficiary: Address, cumulative_payout: U256) -> Self {
        Self {
            chequebook,
            beneficiary,
            cumulativePayout: cumulative_payout,
        }
    }

    fn cumulative_payout(&self) -> U256 {
        self.cumulativePayout
    }

    fn domain(spec: &impl SwarmSpec) -> Eip712Domain {
        eip712_domain! {
            name: DOMAIN_NAME,
            version: DOMAIN_VERSION,
            chain_id: spec.chain().id(),
        }
    }

    fn signing_hash(&self, spec: &impl SwarmSpec) -> B256 {
        self.eip712_signing_hash(&Self::domain(spec))
    }
}

/// A signed cheque ready for transmission or cashing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedCheque {
    /// The unsigned cheque data.
    pub cheque: Cheque,
    /// ECDSA signature (65 bytes: r[32] + s[32] + v[1]).
    pub signature: Bytes,
}

impl SignedCheque {
    /// Create a new signed cheque.
    pub fn new(cheque: Cheque, signature: Bytes) -> Self {
        Self { cheque, signature }
    }

    /// Create a signed cheque from a cheque and signature.
    pub fn from_signature(cheque: Cheque, sig: Signature) -> Self {
        Self {
            cheque,
            signature: Bytes::copy_from_slice(&sig.as_bytes()),
        }
    }

    /// Parse the signature bytes.
    fn parse_signature(&self) -> Result<Signature, ChequeError> {
        if self.signature.len() != 65 {
            return Err(ChequeError::SignatureRecovery(format!(
                "invalid signature length: expected 65, got {}",
                self.signature.len()
            )));
        }

        Signature::try_from(self.signature.as_ref())
            .map_err(|e| ChequeError::SignatureRecovery(format!("invalid signature: {e}")))
    }

    /// Recover the signer address from the signature.
    #[must_use = "signature recovery result should be checked"]
    pub fn recover_signer(&self, spec: &impl SwarmSpec) -> Result<Address, ChequeError> {
        let sig = self.parse_signature()?;
        let hash = self.cheque.signing_hash(spec);

        sig.recover_address_from_prehash(&hash)
            .map_err(|e| ChequeError::SignatureRecovery(format!("recovery failed: {e}")))
    }

    /// Verify that this cheque was signed by the expected owner.
    #[must_use = "cheque verification result should be checked"]
    pub fn verify(&self, owner: Address, spec: &impl SwarmSpec) -> Result<(), ChequeError> {
        let signer = self.recover_signer(spec)?;
        if signer != owner {
            return Err(ChequeError::InvalidSigner {
                expected: owner,
                actual: signer,
            });
        }
        Ok(())
    }

    /// Serialize to JSON bytes for SWAP protocol transmission.
    ///
    /// The output is byte-identical to the live network encoding: PascalCase
    /// keys in declaration order, lowercase `0x`-hex addresses, a bare decimal
    /// JSON number for the payout, and standard base64 for the signature.
    // BEE-COMPAT(TBD): cheque rides the swap wire as JSON inside a protobuf
    // bytes field. The shape below mirrors the reference encoder exactly so
    // peers interoperate; retire once the swap payload moves to a binary codec.
    #[must_use = "serialization result should be checked"]
    pub fn to_json(&self) -> Bytes {
        let mut out = String::with_capacity(256);
        out.push_str("{\"Chequebook\":\"0x");
        out.push_str(&hex::encode(self.cheque.chequebook));
        out.push_str("\",\"Beneficiary\":\"0x");
        out.push_str(&hex::encode(self.cheque.beneficiary));
        out.push_str("\",\"CumulativePayout\":");
        // U256 Display is the canonical base-10 representation with no quotes,
        // matching a JSON number for the full 256-bit range.
        out.push_str(&self.cheque.cumulativePayout.to_string());
        out.push_str(",\"Signature\":\"");
        out.push_str(&base64::encode(&self.signature));
        out.push_str("\"}");
        Bytes::from(out.into_bytes())
    }

    /// Deserialize from JSON bytes produced by [`Self::to_json`] or a conformant
    /// peer.
    ///
    /// The parser is deliberately strict and shape-specific: it extracts the
    /// four known fields by key and rejects anything it does not recognise. It
    /// is not a general JSON parser.
    #[must_use = "deserialization result should be checked"]
    pub fn from_json(data: &[u8]) -> Result<Self, ChequeError> {
        let text = core::str::from_utf8(data)
            .map_err(|_| ChequeError::MalformedJson("not valid utf-8"))?;

        let chequebook = parse_address(extract_string(text, "Chequebook")?, "Chequebook")?;
        let beneficiary = parse_address(extract_string(text, "Beneficiary")?, "Beneficiary")?;
        let cumulative_payout = parse_payout(extract_number(text, "CumulativePayout")?)?;
        let signature = base64::decode(extract_string(text, "Signature")?).ok_or(
            ChequeError::InvalidField {
                field: "Signature",
                reason: "invalid base64",
            },
        )?;

        Ok(Self {
            cheque: Cheque {
                chequebook,
                beneficiary,
                cumulativePayout: cumulative_payout,
            },
            signature: Bytes::from(signature),
        })
    }
}

/// Locate `"<key>":` and return the byte offset of the first value byte after
/// the colon, skipping insignificant whitespace.
fn value_start(text: &str, key: &'static str) -> Result<usize, ChequeError> {
    let needle = {
        let mut s = String::with_capacity(key.len() + 3);
        s.push('"');
        s.push_str(key);
        s.push_str("\":");
        s
    };
    let key_pos = text.find(&needle).ok_or(ChequeError::InvalidField {
        field: key,
        reason: "missing field",
    })?;
    let after = key_pos + needle.len();
    let rest = &text[after..];
    let trimmed = rest.trim_start();
    Ok(after + (rest.len() - trimmed.len()))
}

/// Extract the contents of a JSON string value for the given key (without the
/// surrounding quotes). The known string fields contain no escape sequences.
fn extract_string<'a>(text: &'a str, key: &'static str) -> Result<&'a str, ChequeError> {
    let start = value_start(text, key)?;
    let rest = &text[start..];
    let body = rest.strip_prefix('"').ok_or(ChequeError::InvalidField {
        field: key,
        reason: "expected string value",
    })?;
    let end = body.find('"').ok_or(ChequeError::InvalidField {
        field: key,
        reason: "unterminated string",
    })?;
    let value = &body[..end];
    if value.contains('\\') {
        return Err(ChequeError::InvalidField {
            field: key,
            reason: "unexpected escape sequence",
        });
    }
    Ok(value)
}

/// Extract a bare JSON number token for the given key.
fn extract_number<'a>(text: &'a str, key: &'static str) -> Result<&'a str, ChequeError> {
    let start = value_start(text, key)?;
    let rest = &text[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if end == 0 {
        return Err(ChequeError::InvalidField {
            field: key,
            reason: "expected decimal number",
        });
    }
    Ok(&rest[..end])
}

fn parse_address(value: &str, field: &'static str) -> Result<Address, ChequeError> {
    value
        .parse::<Address>()
        .map_err(|_| ChequeError::InvalidField {
            field,
            reason: "invalid 0x-hex address",
        })
}

fn parse_payout(value: &str) -> Result<U256, ChequeError> {
    value
        .parse::<U256>()
        .map_err(|_| ChequeError::InvalidField {
            field: "CumulativePayout",
            reason: "value out of range or non-decimal",
        })
}

/// Minimal standard-alphabet base64 (`+/`, `=` padding), matching the encoding
/// used on the swap wire. Kept local to avoid a dependency in the wasm cone.
mod base64 {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    /// Map a 6-bit value to its alphabet character. The mask keeps the index in
    /// range, so the lookup is always `Some`.
    fn symbol(sextet: u8) -> char {
        ALPHABET
            .get((sextet & 0x3f) as usize)
            .copied()
            .unwrap_or(b'A') as char
    }

    pub(super) fn encode(input: &[u8]) -> String {
        let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
        for chunk in input.chunks(3) {
            let b0 = chunk.first().copied().unwrap_or(0);
            let b1 = chunk.get(1).copied().unwrap_or(0);
            let b2 = chunk.get(2).copied().unwrap_or(0);

            out.push(symbol(b0 >> 2));
            out.push(symbol((b0 << 4) | (b1 >> 4)));
            if chunk.len() > 1 {
                out.push(symbol((b1 << 2) | (b2 >> 6)));
            } else {
                out.push('=');
            }
            if chunk.len() > 2 {
                out.push(symbol(b2));
            } else {
                out.push('=');
            }
        }
        out
    }

    fn decode_char(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    pub(super) fn decode(input: &str) -> Option<Vec<u8>> {
        let bytes = input.as_bytes();
        if !bytes.len().is_multiple_of(4) {
            return None;
        }
        if bytes.is_empty() {
            return Some(Vec::new());
        }
        let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
        for chunk in bytes.chunks_exact(4) {
            let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
            if pad > 2 {
                return None;
            }
            let mut vals = [0u8; 4];
            for (slot, &c) in vals.iter_mut().zip(chunk.iter()) {
                if c == b'=' {
                    // Padding is consumed by `pad`; a `=` before the padding run
                    // means the slot was already covered, so leave it zero.
                    continue;
                }
                *slot = decode_char(c)?;
            }
            // A `=` anywhere but the trailing run is invalid base64.
            if chunk.iter().take(4 - pad).any(|&c| c == b'=') {
                return None;
            }
            let [v0, v1, v2, v3] = vals;
            let n = (u32::from(v0) << 18)
                | (u32::from(v1) << 12)
                | (u32::from(v2) << 6)
                | u32::from(v3);
            out.push((n >> 16) as u8);
            if pad < 2 {
                out.push((n >> 8) as u8);
            }
            if pad < 1 {
                out.push(n as u8);
            }
        }
        Some(out)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use alloy_signer::SignerSync;
    use alloy_signer_local::PrivateKeySigner;
    use vertex_swarm_spec::init_mainnet;

    fn test_cheque() -> Cheque {
        Cheque::new(
            Address::repeat_byte(0x01),
            Address::repeat_byte(0x02),
            U256::from(1_000_000u64),
        )
    }

    #[test]
    fn test_cheque_creation() {
        let cheque = test_cheque();
        assert_eq!(cheque.chequebook, Address::repeat_byte(0x01));
        assert_eq!(cheque.beneficiary, Address::repeat_byte(0x02));
        assert_eq!(cheque.cumulative_payout(), U256::from(1_000_000u64));
    }

    #[test]
    fn test_domain_uses_chain_id() {
        let spec = init_mainnet();
        let domain = Cheque::domain(&*spec);

        // Mainnet uses Gnosis chain (ID 100)
        assert_eq!(domain.chain_id, Some(U256::from(100u64)));
    }

    #[test]
    fn test_signing_hash_deterministic() {
        let spec = init_mainnet();
        let cheque = test_cheque();

        let hash1 = cheque.signing_hash(&*spec);
        let hash2 = cheque.signing_hash(&*spec);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_sign_and_recover() {
        let spec = init_mainnet();
        let signer = PrivateKeySigner::random();
        let cheque = test_cheque();

        // Sign
        let hash = cheque.signing_hash(&*spec);
        let sig = signer.sign_hash_sync(&hash).unwrap();
        let signed = SignedCheque::from_signature(cheque, sig);

        // Recover and verify
        let recovered = signed.recover_signer(&*spec).unwrap();
        assert_eq!(recovered, signer.address());

        signed.verify(signer.address(), &*spec).unwrap();
    }

    #[test]
    fn test_verify_wrong_signer_fails() {
        let spec = init_mainnet();
        let signer = PrivateKeySigner::random();
        let cheque = test_cheque();

        let hash = cheque.signing_hash(&*spec);
        let sig = signer.sign_hash_sync(&hash).unwrap();
        let signed = SignedCheque::from_signature(cheque, sig);

        let wrong = Address::repeat_byte(0x99);
        assert!(matches!(
            signed.verify(wrong, &*spec),
            Err(ChequeError::InvalidSigner { .. })
        ));
    }

    #[test]
    fn test_json_roundtrip() {
        let cheque = test_cheque();
        let signature = Bytes::from(vec![0u8; 65]);
        let signed = SignedCheque::new(cheque, signature);

        let json = signed.to_json();
        let decoded = SignedCheque::from_json(&json).unwrap();
        assert_eq!(signed, decoded);
    }

    #[test]
    fn test_json_field_shape() {
        // The wire keys are PascalCase, the payout is a bare JSON number, and
        // the signature is base64. This is the live-network shape; the codec is
        // strict about it for interoperability.
        let signed = SignedCheque::new(test_cheque(), Bytes::from(vec![0u8; 65]));
        let json = String::from_utf8(signed.to_json().to_vec()).unwrap();
        assert!(json.contains("\"CumulativePayout\":1000000"));
        assert!(json.contains("\"Chequebook\":\"0x"));
        assert!(json.contains("\"Beneficiary\":\"0x"));
        assert!(json.contains("\"Signature\":\""));
    }
}
