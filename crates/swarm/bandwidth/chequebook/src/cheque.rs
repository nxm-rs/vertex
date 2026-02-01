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

use alloy_primitives::{Address, B256, Signature, U256};
use alloy_sol_types::{Eip712Domain, SolStruct, eip712_domain};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use vertex_swarmspec::SwarmSpec;

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedCheque {
    /// The unsigned cheque data.
    #[serde(flatten)]
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
    pub fn recover_signer(&self, spec: &impl SwarmSpec) -> Result<Address, ChequeError> {
        let sig = self.parse_signature()?;
        let hash = self.cheque.signing_hash(spec);

        sig.recover_address_from_prehash(&hash)
            .map_err(|e| ChequeError::SignatureRecovery(format!("recovery failed: {e}")))
    }

    /// Verify that this cheque was signed by the expected owner.
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
    pub fn to_json(&self) -> Result<Bytes, ChequeError> {
        serde_json::to_vec(self)
            .map(Bytes::from)
            .map_err(|e| ChequeError::Serialization(e.to_string()))
    }

    /// Deserialize from JSON bytes.
    pub fn from_json(data: &[u8]) -> Result<Self, ChequeError> {
        serde_json::from_slice(data).map_err(|e| ChequeError::Serialization(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_signer::SignerSync;
    use alloy_signer_local::PrivateKeySigner;
    use vertex_swarmspec::init_mainnet;

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
    fn test_json_uses_camel_case() {
        let signed = SignedCheque::new(test_cheque(), Bytes::from(vec![0u8; 65]));
        let json = serde_json::to_string(&signed).unwrap();
        assert!(json.contains("cumulativePayout"));
    }
}
