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
//!
//! # Transport encoding
//!
//! A [`SignedCheque`] travels on the swap protocol as a JSON object embedded in
//! a protobuf `bytes` field. The JSON is transport-only: the cheque signature is
//! the EIP-712 typed-data signature over the cheque fields (`chequebook`,
//! `beneficiary`, `cumulativePayout`) under the `Chequebook` domain, not over the
//! JSON bytes. Verification rebuilds the EIP-712 hash and recovers against it
//! (see [`SignedCheque::recover_signer`]), so the JSON only needs to be
//! cross-implementation parseable and to round-trip the field values; it does not
//! need to be byte-identical. The whole JSON path is slated for protobuf
//! replacement, tracked in issue #183.
//!
//! For parseability the encoding follows the field-value conventions other
//! Swarm nodes emit: PascalCase keys, lowercase `0x`-hex addresses,
//! `CumulativePayout` as a bare decimal JSON number across the full 256-bit
//! range, and `Signature` as standard base64.

use alloy_primitives::{Address, B256, Signature, U256};
use alloy_sol_types::{Eip712Domain, SolStruct, eip712_domain};
use bytes::Bytes;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
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
///
/// Serde is implemented for the swap transport via [`WireCheque`], so callers can
/// encode and decode with `serde_json` directly. The serde representation matches
/// the field-value conventions documented at the module root (PascalCase keys,
/// lowercase `0x`-hex addresses, bare-number payout, base64 signature). The JSON
/// is transport-only: signatures are EIP-712 over the cheque fields, not over the
/// JSON bytes, so the encoding does not need to be byte-identical to any peer,
/// only parseable and value-preserving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedCheque {
    /// The unsigned cheque data.
    pub cheque: Cheque,
    /// The raw signature payload (canonically 65 bytes: r[32] + s[32] + v[1]).
    ///
    /// Kept as opaque [`Bytes`] rather than a typed
    /// [`alloy_primitives::Signature`] on purpose. A typed `Signature` fixes the
    /// length at 65 bytes, rejects non-canonical `v` values, and renormalizes
    /// `v` to `27 + parity` on re-encode. The opaque payload preserves whatever
    /// a peer sends and is parsed into a `Signature` only when verifying or
    /// recovering a signer (see [`Self::recover_signer`]).
    pub signature: Bytes,
}

impl Serialize for SignedCheque {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        WireCheque {
            chequebook: self.cheque.chequebook,
            beneficiary: self.cheque.beneficiary,
            cumulative_payout: self.cheque.cumulativePayout,
            signature: self.signature.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SignedCheque {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = WireCheque::deserialize(deserializer)?;
        Ok(Self {
            cheque: Cheque {
                chequebook: wire.chequebook,
                beneficiary: wire.beneficiary,
                cumulativePayout: wire.cumulative_payout,
            },
            signature: wire.signature,
        })
    }
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
    /// Thin wrapper over `serde_json::to_vec`. The encoding follows the
    /// field-value conventions documented at the module root.
    #[must_use = "serialization result should be checked"]
    pub fn to_json(&self) -> Result<Bytes, ChequeError> {
        let bytes = serde_json::to_vec(self).map_err(|_| ChequeError::Encode("serialize"))?;
        Ok(Bytes::from(bytes))
    }

    /// Deserialize from JSON bytes produced by [`Self::to_json`] or a conformant
    /// peer.
    ///
    /// Thin wrapper over `serde_json::from_slice`.
    #[must_use = "deserialization result should be checked"]
    pub fn from_json(data: &[u8]) -> Result<Self, ChequeError> {
        serde_json::from_slice(data)
            .map_err(|_| ChequeError::MalformedJson("not a signed cheque object"))
    }
}

/// The on-wire JSON shape of a signed cheque.
///
/// Derives serde with PascalCase keys to match the field-value conventions other
/// Swarm nodes emit. Addresses serialize as lowercase `0x`-hex, `CumulativePayout`
/// as a bare decimal JSON number across the full 256-bit range, and `Signature`
/// as standard base64. The JSON is transport-only, so this shape only needs to be
/// cross-implementation parseable and value-preserving, not byte-pinned.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WireCheque {
    // `Address`'s alloy serde emits a lowercase `0x`-hex string and parses any
    // case, matching the field-value conventions other Swarm nodes emit, so the
    // default serde needs no helper.
    chequebook: Address,
    beneficiary: Address,
    #[serde(with = "payout_number")]
    cumulative_payout: U256,
    #[serde(with = "signature_base64")]
    signature: Bytes,
}

/// Bare decimal JSON number serde for `U256`.
///
/// `U256` exceeds `u64`, so a [`RawValue`] carries the decimal token. The decoder
/// rejects anything that is not a bare decimal number (quoted strings, signs,
/// decimal points, exponents), keeping the payout shape strict.
mod payout_number {
    use alloy_primitives::U256;
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
    use serde_json::value::RawValue;

    pub(super) fn serialize<S: Serializer>(value: &U256, s: S) -> Result<S::Ok, S::Error> {
        let raw = RawValue::from_string(value.to_string()).map_err(serde::ser::Error::custom)?;
        raw.serialize(s)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<U256, D::Error> {
        let raw = Box::<RawValue>::deserialize(d)?;
        let token = raw.get();
        if token.is_empty() || !token.bytes().all(|b| b.is_ascii_digit()) {
            return Err(de::Error::custom("expected a bare decimal number"));
        }
        token.parse::<U256>().map_err(de::Error::custom)
    }
}

/// Standard base64 serde for the signature payload.
///
/// `Vec<u8>` serializes as a JSON number array by default; base64 matches the
/// `[]byte` encoding other Swarm nodes emit.
mod signature_base64 {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use bytes::Bytes;
    use serde::{Deserialize, Deserializer, Serializer, de};

    pub(super) fn serialize<S: Serializer>(sig: &Bytes, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&BASE64.encode(sig))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Bytes, D::Error> {
        let s = <&str>::deserialize(d)?;
        BASE64
            .decode(s.as_bytes())
            .map(Bytes::from)
            .map_err(de::Error::custom)
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

    #[test]
    fn test_address_fields_are_lowercase_hex() {
        // Use addresses whose checksummed form has uppercase hex digits so a
        // future alloy switch to EIP-55 mixed case would fail this assertion.
        let cheque = Cheque::new(
            "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd"
                .parse()
                .unwrap(),
            "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
                .parse()
                .unwrap(),
            U256::from(1u64),
        );
        let signed = SignedCheque::new(cheque, Bytes::from(vec![0u8; 65]));
        let json = String::from_utf8(signed.to_json().unwrap().to_vec()).unwrap();
        assert!(json.contains("\"Chequebook\":\"0xabcdefabcdefabcdefabcdefabcdefabcdefabcd\""));
        assert!(json.contains("\"Beneficiary\":\"0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef\""));
    }

    #[test]
    fn test_checksummed_address_deserializes() {
        // A peer emitting EIP-55 mixed-case addresses must still parse, since
        // address parsing is case-insensitive.
        let json = r#"{"Chequebook":"0xAbCdEfAbCdEfAbCdEfAbCdEfAbCdEfAbCdEfAbCd","Beneficiary":"0xDeAdBeEfDeAdBeEfDeAdBeEfDeAdBeEfDeAdBeEf","CumulativePayout":1,"Signature":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="}"#;
        let decoded = SignedCheque::from_json(json.as_bytes()).unwrap();
        assert_eq!(
            decoded.cheque.chequebook,
            "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd"
                .parse::<Address>()
                .unwrap()
        );
        assert_eq!(
            decoded.cheque.beneficiary,
            "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
                .parse::<Address>()
                .unwrap()
        );
    }
}
