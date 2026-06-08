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
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
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
    /// The raw signature payload (canonically 65 bytes: r[32] + s[32] + v[1]).
    ///
    /// Kept as opaque [`Bytes`] rather than a typed
    /// [`alloy_primitives::Signature`] on purpose. The wire format is base64 of
    /// whatever signature payload the peer sends, and the codec must round-trip
    /// it byte-for-byte. A typed `Signature` fixes the length at 65 bytes,
    /// rejects non-canonical `v` values, and renormalizes `v` to `27 + parity`
    /// on re-encode, any of which would change the bytes and break wire
    /// equivalence. The payload is parsed into a `Signature` only when verifying
    /// or recovering a signer (see [`Self::recover_signer`]).
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
    #[must_use = "serialization result should be checked"]
    pub fn to_json(&self) -> Result<Bytes, ChequeError> {
        let payout = self.cheque.cumulativePayout.to_string();
        // `CumulativePayout` is a bare JSON number spanning the full 256-bit
        // range, so it cannot ride in a `u64`. Inject the decimal as a raw
        // token; the decimal string from `U256::to_string` is valid JSON.
        let payout = RawValue::from_string(payout)
            .map_err(|_| ChequeError::Encode("payout is not a valid json number"))?;

        let wire = WireCheque {
            chequebook: lower_hex(self.cheque.chequebook),
            beneficiary: lower_hex(self.cheque.beneficiary),
            cumulative_payout: payout,
            signature: BASE64.encode(&self.signature),
        };

        let bytes = serde_json::to_vec(&wire).map_err(|_| ChequeError::Encode("serialize"))?;
        Ok(Bytes::from(bytes))
    }

    /// Deserialize from JSON bytes produced by [`Self::to_json`] or a conformant
    /// peer.
    ///
    /// Parsing is strict about the wire shape: the payout must be a bare JSON
    /// number, the addresses must be `0x`-hex, and the signature must be
    /// standard base64.
    #[must_use = "deserialization result should be checked"]
    pub fn from_json(data: &[u8]) -> Result<Self, ChequeError> {
        let wire: WireCheque = serde_json::from_slice(data)
            .map_err(|_| ChequeError::MalformedJson("not a signed cheque object"))?;

        let chequebook = parse_address(&wire.chequebook, "Chequebook")?;
        let beneficiary = parse_address(&wire.beneficiary, "Beneficiary")?;
        let cumulative_payout = parse_payout(wire.cumulative_payout.get())?;
        let signature =
            BASE64
                .decode(wire.signature.as_bytes())
                .map_err(|_| ChequeError::InvalidField {
                    field: "Signature",
                    reason: "invalid base64",
                })?;

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

/// The on-wire JSON shape of a signed cheque.
///
/// The field order and PascalCase rename pin the byte layout produced on the
/// swap wire. `serde_json` preserves struct field order, so the encoder output
/// matches the live network exactly. `CumulativePayout` is a [`RawValue`] so it
/// stays a bare JSON number across the full 256-bit range; a quoted string is
/// rejected on decode by [`parse_payout`].
#[derive(Serialize, Deserialize)]
struct WireCheque {
    #[serde(rename = "Chequebook")]
    chequebook: String,
    #[serde(rename = "Beneficiary")]
    beneficiary: String,
    #[serde(rename = "CumulativePayout")]
    cumulative_payout: Box<RawValue>,
    #[serde(rename = "Signature")]
    signature: String,
}

/// Format an address as lowercase `0x`-hex, matching the swap wire encoding.
///
/// `Address`'s `Display` emits an EIP-55 mixed-case checksum, so the lowercase
/// form is produced explicitly here.
fn lower_hex(address: Address) -> String {
    let mut s = String::with_capacity(42);
    s.push_str("0x");
    s.push_str(&hex::encode(address));
    s
}

fn parse_address(value: &str, field: &'static str) -> Result<Address, ChequeError> {
    value
        .parse::<Address>()
        .map_err(|_| ChequeError::InvalidField {
            field,
            reason: "invalid 0x-hex address",
        })
}

/// Parse the `CumulativePayout` raw JSON token as a `U256`.
///
/// The token must be a bare decimal number. A quoted string, a sign, a decimal
/// point, or an exponent all fail here, which keeps the decoder strict about the
/// wire shape.
fn parse_payout(value: &str) -> Result<U256, ChequeError> {
    if !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ChequeError::InvalidField {
            field: "CumulativePayout",
            reason: "expected a bare decimal number",
        });
    }
    value
        .parse::<U256>()
        .map_err(|_| ChequeError::InvalidField {
            field: "CumulativePayout",
            reason: "value out of range",
        })
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

        let json = signed.to_json().unwrap();
        let decoded = SignedCheque::from_json(&json).unwrap();
        assert_eq!(signed, decoded);
    }

    #[test]
    fn test_json_field_shape() {
        // The wire keys are PascalCase, the payout is a bare JSON number, and
        // the signature is base64. This is the live-network shape; the codec is
        // strict about it for interoperability.
        let signed = SignedCheque::new(test_cheque(), Bytes::from(vec![0u8; 65]));
        let json = String::from_utf8(signed.to_json().unwrap().to_vec()).unwrap();
        assert!(json.contains("\"CumulativePayout\":1000000"));
        assert!(json.contains("\"Chequebook\":\"0x"));
        assert!(json.contains("\"Beneficiary\":\"0x"));
        assert!(json.contains("\"Signature\":\""));
    }
}
