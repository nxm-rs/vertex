//! The verified custody receipt produced at the pushsync decode boundary.
//!
//! [`SignedReceipt`] is the only receipt representation allowed to exist past
//! the pushsync decode boundary. There is deliberately no "unsigned receipt"
//! type: a structurally-decoded [`Receipt`](crate::Receipt) whose signer cannot
//! be recovered is rejected at decode (before it reaches any handler, forwarder,
//! or origin path), so the responding peer can be scored for it and no domain
//! code ever holds an unverified receipt.
//!
//! # Why the signer is recovered, not read
//!
//! A custody receipt's `storer` is not on the wire. The storer signs over the
//! 32-byte chunk address only; the nonce is a separate field that binds the
//! signer's overlay, not the signature. On a multi-hop relay the immediate peer
//! that hands a receipt back is several hops from the real signer, so the signer
//! must be recovered from the signature, never read from whoever delivered the
//! receipt. [`SignedReceipt::recover`] is the sole constructor and performs that
//! recovery; the recovered overlay is then the input to the depth policy in
//! `vertex-swarm-api`.
//!
//! # Future wire format
//!
//! When the wire format is later refined to carry a self-authenticating signer
//! (tracked under a separate fork-gated proposal), only [`SignedReceipt::recover`]
//! changes: the [`SignedReceipt`] type and every domain consumer that depends on
//! its recovered [`signer`](SignedReceipt::signer) stay put. The recovery itself
//! has no topology or database dependency (only the signature, the network id,
//! and `compute_overlay`); a separate issue tracks lifting it into nectar as a
//! reusable primitive, at which point this constructor delegates to it.

use nectar_primitives::ChunkAddress;
use vertex_swarm_primitives::{NetworkId, Nonce, OverlayAddress, StorageRadius, compute_overlay};

use crate::codec::Receipt;
use crate::error::PushsyncError;

/// Length in bytes of a well-formed recoverable signature.
const SIGNATURE_LEN: usize = 65;

/// A custody receipt whose signer has been recovered and verified.
///
/// Constructed only by [`SignedReceipt::recover`], so its [`signer`] field is
/// always a real overlay recovered from the signature. The signature and nonce
/// are retained so a forwarder can relay the receipt verbatim (it never re-signs
/// or mints a receipt); the recovered signer is what the depth policy consumes.
///
/// [`signer`]: SignedReceipt::signer
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedReceipt {
    /// The address of the chunk the receipt acknowledges custody of.
    address: ChunkAddress,
    /// The overlay recovered from the signature over the chunk address.
    signer: OverlayAddress,
    /// The signer's signature over the 32-byte chunk address.
    signature: alloy_primitives::Signature,
    /// The nonce the signer used to derive its overlay.
    nonce: Nonce,
    /// The signer's declared storage radius at the time of acceptance.
    storage_radius: StorageRadius,
}

impl SignedReceipt {
    /// Recover and verify the signer of a structurally-decoded receipt.
    ///
    /// The signed message is the 32-byte chunk address only; the nonce binds the
    /// overlay, not the signature. The signer overlay is derived from the
    /// recovered Ethereum address with the canonical
    /// [`compute_overlay`](vertex_swarm_primitives::compute_overlay) formula
    /// (`keccak256(eth || network_id_le || nonce)`).
    ///
    /// Returns [`PushsyncError::MalformedReceiptSignature`] for an all-zero
    /// (structural failure) signature or any signature that fails recovery.
    /// There is no other way to build a [`SignedReceipt`], so a receipt that
    /// reaches a domain consumer is always signer-verified.
    pub fn recover(receipt: Receipt, network_id: NetworkId) -> Result<Self, PushsyncError> {
        // The structural failure signal is an all-zero signature; never recover
        // it. (The codec already rejects a wrong-length signature as a failure
        // response, so a `Receipt` here is full-length.)
        if receipt.signature.as_bytes() == [0u8; SIGNATURE_LEN] {
            return Err(PushsyncError::MalformedReceiptSignature);
        }

        let eth = receipt
            .signature
            .recover_address_from_msg(receipt.address.as_bytes())
            .map_err(|_| PushsyncError::MalformedReceiptSignature)?;
        let signer = compute_overlay(&eth, network_id, &receipt.nonce);

        Ok(Self {
            address: receipt.address,
            signer,
            signature: receipt.signature,
            nonce: receipt.nonce,
            storage_radius: receipt.storage_radius,
        })
    }

    /// The chunk address the receipt acknowledges custody of.
    #[must_use]
    pub fn address(&self) -> &ChunkAddress {
        &self.address
    }

    /// The overlay recovered from the signature: the real signer, regardless of
    /// which peer relayed the receipt.
    #[must_use]
    pub fn signer(&self) -> OverlayAddress {
        self.signer
    }

    /// The signer's signature over the chunk address.
    #[must_use]
    pub fn signature(&self) -> &alloy_primitives::Signature {
        &self.signature
    }

    /// The nonce the signer used to derive its overlay.
    #[must_use]
    pub fn nonce(&self) -> &Nonce {
        &self.nonce
    }

    /// The signer's declared storage radius.
    #[must_use]
    pub fn storage_radius(&self) -> StorageRadius {
        self.storage_radius
    }

    /// Rebuild the wire receipt for verbatim relay.
    ///
    /// A forwarder relays the storer's receipt unchanged; it never re-signs.
    /// This reproduces the exact signature, nonce, and radius the signer
    /// produced, addressed to the chunk under relay.
    #[must_use]
    pub fn to_wire(&self) -> Receipt {
        Receipt::success(
            self.address,
            self.signature,
            self.nonce,
            self.storage_radius,
        )
    }
}

#[cfg(test)]
mod tests {
    use alloy_signer::SignerSync;
    use alloy_signer_local::PrivateKeySigner;
    use nectar_primitives::Bin;

    use super::*;

    const NET: NetworkId = NetworkId::MAINNET;

    fn chunk_address(first_byte: u8) -> ChunkAddress {
        let mut bytes = [0u8; 32];
        bytes[0] = first_byte;
        ChunkAddress::new(bytes)
    }

    fn radius(n: u8) -> StorageRadius {
        StorageRadius::new(Bin::new(n).unwrap())
    }

    /// Sign over the 32-byte chunk address and grind the nonce until the signer
    /// overlay sits at least `min_depth` bits deep relative to `address`.
    fn signed(
        signer: &PrivateKeySigner,
        address: &ChunkAddress,
        min_depth: u8,
    ) -> (Receipt, OverlayAddress) {
        let eth = signer.address();
        let signature = signer.sign_message_sync(address.as_bytes()).expect("sign");
        let mut counter = 0u64;
        loop {
            let mut nonce_bytes = [0u8; 32];
            nonce_bytes[..8].copy_from_slice(&counter.to_le_bytes());
            let nonce = Nonce::from(nonce_bytes);
            let overlay = compute_overlay(&eth, NET, &nonce);
            if address.proximity(&overlay).get() >= min_depth {
                return (
                    Receipt::success(*address, signature, nonce, radius(8)),
                    overlay,
                );
            }
            counter += 1;
        }
    }

    #[test]
    fn recovers_the_signer_overlay_from_the_signature() {
        let signer = PrivateKeySigner::random();
        let address = chunk_address(0xff);
        let (receipt, expected) = signed(&signer, &address, 8);
        let signed_receipt = SignedReceipt::recover(receipt, NET).expect("recovers");
        assert_eq!(
            signed_receipt.signer(),
            expected,
            "recovers the real signer"
        );
        assert_eq!(*signed_receipt.address(), address);
        assert_eq!(signed_receipt.storage_radius(), radius(8));
    }

    #[test]
    fn relays_verbatim_via_to_wire() {
        let signer = PrivateKeySigner::random();
        let address = chunk_address(0xff);
        let (receipt, _) = signed(&signer, &address, 8);
        let signed_receipt = SignedReceipt::recover(receipt.clone(), NET).expect("recovers");
        assert_eq!(
            signed_receipt.to_wire(),
            receipt,
            "the relayed receipt is byte-identical to the storer's"
        );
    }

    #[test]
    fn all_zero_signature_is_rejected_at_decode() {
        let address = chunk_address(0xff);
        let receipt = Receipt::success(
            address,
            alloy_primitives::Signature::from_raw(&[0u8; 65]).expect("zero signature parses"),
            Nonce::from([7u8; 32]),
            radius(8),
        );
        let err = SignedReceipt::recover(receipt, NET).expect_err("zero signature rejected");
        assert!(matches!(err, PushsyncError::MalformedReceiptSignature));
    }

    #[test]
    fn signature_over_a_different_message_recovers_a_different_signer() {
        // The signature is over the 32-byte address only; a signature produced
        // over a different address recovers a different ethereum address, hence a
        // different overlay. The depth policy (in vertex-swarm-api) then rejects
        // such a receipt as shallow for the real chunk.
        let signer = PrivateKeySigner::random();
        let real = chunk_address(0xff);
        let other = chunk_address(0x00);
        let (mut receipt, _) = signed(&signer, &other, 8);
        // Re-address the receipt to `real` but keep the signature over `other`.
        receipt.address = real;
        let recovered = SignedReceipt::recover(receipt, NET).expect("recovers to something");
        let honest = signed(&signer, &real, 8).1;
        assert_ne!(
            recovered.signer(),
            honest,
            "a signature over a different message recovers a different overlay"
        );
    }
}
