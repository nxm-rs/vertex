//! The custody receipt: reconstructed and verified at the decode boundary.
//!
//! [`Receipt`] is the only receipt representation that exists past the pushsync
//! decode boundary. An unsigned receipt is not representable: the sole
//! constructor [`Receipt::reconstruct`] recovers the storer overlay from the
//! signature, so a receipt whose storer cannot be recovered is rejected at decode
//! (as [`PushsyncError::MalformedReceiptSignature`]) and never becomes a
//! [`Receipt`]. Domain code therefore deals only in receipts whose `storer` is a
//! real, recovered overlay.
//!
//! # Why the storer is recovered, not read
//!
//! A receipt's storer is not on the wire. The storer signs over the 32-byte chunk
//! address only; the nonce is a separate field that binds the storer's overlay,
//! not the signature. On a multi-hop relay the immediate peer that hands a
//! receipt back is several hops from the real storer, so the storer is recovered
//! from the signature, never read from whoever delivered the receipt.
//!
//! # Depth policy
//!
//! [`Receipt::verify_depth`] is the custody-depth check: a receipt is trusted
//! only when its storer is deep enough for the chunk, with the bar derived from
//! the locally observed neighbourhood depth and trust-but-verified against the
//! receipt's own declared radius. The recovery and the check both live next to
//! the type so consumers call `receipt.verify_depth(local_depth)` directly.
//!
//! # Future wire format
//!
//! When the wire format is later refined to carry a self-authenticating storer
//! (tracked under a separate fork-gated proposal), only [`Receipt::reconstruct`]
//! changes: the [`Receipt`] type and every consumer stay put. The recovery has no
//! topology or database dependency (only the signature, the network id, and
//! `compute_overlay`); a separate issue tracks lifting it into nectar as a
//! reusable primitive.

use nectar_primitives::ChunkAddress;
use vertex_swarm_primitives::{
    NeighborhoodDepth, NetworkId, Nonce, OverlayAddress, StorageRadius, compute_overlay,
};

use crate::codec::WireReceipt;
use crate::error::PushsyncError;

/// Length in bytes of a well-formed recoverable signature.
const SIGNATURE_LEN: usize = 65;

/// Slack subtracted from the locally observed depth before it becomes the floor.
///
/// Topology views drift: a peer at the genuine edge of the responsible
/// neighbourhood can sit one bin shallower than our own observed depth without
/// being a free-rider. Subtracting a small fixed tolerance from the local floor
/// avoids false-rejecting such an honest, marginally-shallow receipt while still
/// rejecting receipts that are meaningfully outside the neighbourhood. The
/// declared radius can only raise the bar above this floor, never lower it.
pub const SHALLOW_RECEIPT_TOLERANCE: u8 = 1;

/// The storer of a custody receipt is too shallow for the chunk:
/// `PO(storer, chunk) < required`. The chunk never reached the responsible
/// neighbourhood, so the custody claim is fraudulent (or, at best, useless for
/// retrievability) and must be rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("shallow receipt: storer proximity {observed} below required depth {required}")]
pub struct ShallowReceipt {
    /// The required depth the storer had to reach.
    pub required: u8,
    /// The storer's actual proximity order to the chunk address.
    pub observed: u8,
}

impl From<&ShallowReceipt> for &'static str {
    /// The metric label for a shallow-receipt rejection. A static label paired
    /// with the typed `required`/`observed` fields gives metrics a clean
    /// `reason` value without leaking the dynamic depths into the label space.
    fn from(_: &ShallowReceipt) -> Self {
        "shallow_receipt"
    }
}

/// A custody receipt whose storer has been recovered and verified.
///
/// Constructed only by [`Receipt::reconstruct`], so [`storer`](Self::storer) is
/// always a real overlay recovered from the signature. The signature and nonce
/// are retained so a forwarder can relay the receipt verbatim (it never re-signs
/// or mints a receipt); the recovered storer is what the depth policy consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// The address of the chunk the receipt acknowledges custody of.
    pub address: ChunkAddress,
    /// The overlay recovered from the signature: the real storer, regardless of
    /// which peer relayed the receipt.
    pub storer: OverlayAddress,
    /// The storer's signature over the 32-byte chunk address.
    pub signature: alloy_primitives::Signature,
    /// The nonce the storer used to derive its overlay.
    pub nonce: Nonce,
    /// The storer's declared storage radius at the time of acceptance.
    pub storage_radius: StorageRadius,
}

impl Receipt {
    /// Reconstruct and verify the storer of a wire receipt at the decode
    /// boundary.
    ///
    /// The signed message is the 32-byte chunk address only; the nonce binds the
    /// overlay, not the signature. The storer overlay is derived from the
    /// recovered Ethereum address with the canonical
    /// [`compute_overlay`](vertex_swarm_primitives::compute_overlay) formula
    /// (`keccak256(eth || network_id_le || nonce)`).
    ///
    /// Returns [`PushsyncError::MalformedReceiptSignature`] for an all-zero
    /// (structural failure) signature or any signature that fails recovery. This
    /// is the only way to build a [`Receipt`], so a receipt that reaches a
    /// consumer is always storer-verified.
    pub fn reconstruct(wire: WireReceipt, network_id: NetworkId) -> Result<Self, PushsyncError> {
        // The structural failure signal is an all-zero signature; never recover
        // it. (The codec already rejects a wrong-length signature as a failure
        // response, so a `WireReceipt` here is full-length.)
        if wire.signature.as_bytes() == [0u8; SIGNATURE_LEN] {
            return Err(PushsyncError::MalformedReceiptSignature);
        }

        let eth = wire
            .signature
            .recover_address_from_msg(wire.address.as_bytes())
            .map_err(|_| PushsyncError::MalformedReceiptSignature)?;
        let storer = compute_overlay(&eth, network_id, &wire.nonce);

        Ok(Self {
            address: wire.address,
            storer,
            signature: wire.signature,
            nonce: wire.nonce,
            storage_radius: wire.storage_radius,
        })
    }

    /// Verify the storer is deep enough for the chunk.
    ///
    /// The required depth is dynamic: hard-coding it would reject legitimately
    /// shallow receipts in a small, young, or sparse neighbourhood, failing valid
    /// uploads. The locally observed neighbourhood depth is the trusted authority
    /// (the only input an attacker cannot influence), so it sets the floor (minus
    /// a small [`SHALLOW_RECEIPT_TOLERANCE`] for honest topology drift). The
    /// receipt's declared `storage_radius` is attacker-controlled and is trusted
    /// only to raise the bar, never to lower it: a storer that declares a deeper
    /// radius binds itself to a stricter custody claim, while a shallower (or
    /// zero) radius cannot weaken the local floor. Hence
    /// `required = max(local_depth - tolerance, storage_radius)`.
    ///
    /// The depth check is a cheap first filter, not a cryptographic guarantee;
    /// stake-binding and retrievability auditing (storage incentives, tracked in
    /// #75) are the layer that makes forgery unprofitable.
    pub fn verify_depth(&self, local_depth: NeighborhoodDepth) -> Result<(), ShallowReceipt> {
        let floor = local_depth.get().saturating_sub(SHALLOW_RECEIPT_TOLERANCE);
        let required = floor.max(self.storage_radius.get());
        let observed = self.address.proximity(&self.storer).get();
        if observed < required {
            return Err(ShallowReceipt { required, observed });
        }
        Ok(())
    }

    /// Reproduce the wire receipt for verbatim relay.
    ///
    /// A forwarder relays the storer's receipt unchanged; it never re-signs. The
    /// recovered `storer` is not on the wire, so this reproduces only the bytes
    /// the storer signed (the signature, nonce, and radius for the chunk).
    #[must_use]
    pub fn to_wire(&self) -> WireReceipt {
        WireReceipt::new(
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

    fn depth(n: u8) -> NeighborhoodDepth {
        NeighborhoodDepth::new(Bin::new(n).unwrap())
    }

    fn radius(n: u8) -> StorageRadius {
        StorageRadius::new(Bin::new(n).unwrap())
    }

    /// Sign over the 32-byte chunk address and grind the nonce until the storer
    /// overlay sits at least `min_depth` bits deep relative to `address`.
    fn wire(
        signer: &PrivateKeySigner,
        address: &ChunkAddress,
        min_depth: u8,
        storage_radius: StorageRadius,
    ) -> (WireReceipt, OverlayAddress) {
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
                    WireReceipt::new(*address, signature, nonce, storage_radius),
                    overlay,
                );
            }
            counter += 1;
        }
    }

    #[test]
    fn reconstructs_the_storer_overlay_from_the_signature() {
        let signer = PrivateKeySigner::random();
        let address = chunk_address(0xff);
        let (raw, expected) = wire(&signer, &address, 8, radius(8));
        let receipt = Receipt::reconstruct(raw, NET).expect("reconstructs");
        assert_eq!(receipt.storer, expected, "recovers the real storer");
        assert_eq!(receipt.address, address);
        assert_eq!(receipt.storage_radius, radius(8));
    }

    #[test]
    fn relays_verbatim_via_to_wire() {
        let signer = PrivateKeySigner::random();
        let address = chunk_address(0xff);
        let (raw, _) = wire(&signer, &address, 8, radius(8));
        let receipt = Receipt::reconstruct(raw.clone(), NET).expect("reconstructs");
        assert_eq!(
            receipt.to_wire(),
            raw,
            "the relayed receipt is byte-identical to the storer's"
        );
    }

    #[test]
    fn all_zero_signature_is_rejected_at_decode() {
        let address = chunk_address(0xff);
        let raw = WireReceipt::new(
            address,
            alloy_primitives::Signature::from_raw(&[0u8; 65]).expect("zero signature parses"),
            Nonce::from([7u8; 32]),
            radius(8),
        );
        let err = Receipt::reconstruct(raw, NET).expect_err("zero signature rejected");
        assert!(matches!(err, PushsyncError::MalformedReceiptSignature));
    }

    #[test]
    fn deep_enough_receipt_passes_the_depth_check() {
        let signer = PrivateKeySigner::random();
        let address = chunk_address(0xff);
        let (raw, _) = wire(&signer, &address, 8, radius(8));
        let receipt = Receipt::reconstruct(raw, NET).expect("reconstructs");
        receipt
            .verify_depth(depth(8))
            .expect("deep enough accepted");
    }

    #[test]
    fn shallow_receipt_is_rejected_and_radius_zero_cannot_lower_the_bar() {
        let signer = PrivateKeySigner::random();
        let address = chunk_address(0xff);
        // The storer is shallow (grind only to depth 0) and declares radius 0 to
        // try to bypass the check; the local floor (depth 12) rejects it anyway.
        let (raw, storer) = wire(&signer, &address, 0, radius(0));
        let observed = address.proximity(&storer).get();
        let local = observed + SHALLOW_RECEIPT_TOLERANCE + 1;
        let receipt = Receipt::reconstruct(raw, NET).expect("reconstructs");
        let err = receipt
            .verify_depth(depth(local))
            .expect_err("radius 0 does not bypass the local floor");
        assert_eq!(err.observed, observed);
        assert!(err.required > observed);
    }

    #[test]
    fn declared_radius_can_only_raise_the_bar() {
        let signer = PrivateKeySigner::random();
        let address = chunk_address(0xff);
        // A storer ~4 bits deep declaring radius 12 binds itself to a stricter
        // claim; it is rejected even where the local floor alone would accept it.
        let (raw, storer) = wire(&signer, &address, 4, radius(12));
        let observed = address.proximity(&storer).get();
        assert!(
            observed < 12,
            "constructed storer sits below the declared radius"
        );
        let receipt = Receipt::reconstruct(raw, NET).expect("reconstructs");
        let err = receipt
            .verify_depth(depth(2))
            .expect_err("below-declared-radius storer rejected");
        assert_eq!(
            err,
            ShallowReceipt {
                required: 12,
                observed
            }
        );
    }
}
