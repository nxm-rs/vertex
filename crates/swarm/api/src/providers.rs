//! RPC provider traits for Swarm protocol.
//!
//! Data interfaces for RPC services, abstracting over concrete implementations.

use alloy_primitives::Signature;
use nectar_primitives::{ChunkAddress, NetworkId, Nonce, compute_overlay};
use vertex_swarm_primitives::{NeighborhoodDepth, OverlayAddress, StampedChunk, StorageRadius};

use crate::SwarmResult;

/// Result of a successful chunk retrieval.
#[derive(Debug, Clone)]
pub struct ChunkRetrievalResult {
    /// The retrieved chunk and its postage stamp.
    pub chunk: StampedChunk,
    /// Overlay address of the peer that served this chunk.
    pub served_by: OverlayAddress,
}

/// Provider trait for chunk retrieval operations.
#[async_trait::async_trait]
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait SwarmChunkProvider: Send + Sync + 'static {
    /// Retrieve a chunk by its address from the Swarm network.
    async fn retrieve_chunk(&self, address: &ChunkAddress) -> SwarmResult<ChunkRetrievalResult>;

    /// Check if a chunk exists locally.
    ///
    /// Returns false for Clients, which have no local storage.
    fn has_chunk(&self, address: &ChunkAddress) -> bool;
}

/// Receipt for a chunk accepted by a storer via PushSync.
#[derive(Debug, Clone)]
pub struct PushReceipt {
    /// Overlay address of the storer that accepted this chunk.
    pub storer: OverlayAddress,
    /// The storer's signature over the receipt.
    pub signature: Signature,
    /// The nonce used by the storer in signing.
    pub nonce: Nonce,
    /// The storer's storage radius at the time of acceptance.
    pub storage_radius: StorageRadius,
}

/// Why a custody receipt failed depth verification.
///
/// A receipt is a statement of custody by a node claiming to sit inside the
/// chunk's neighbourhood. Two ways it can be rejected before it is trusted or
/// relayed: its signature does not recover to an overlay at all (malformed), or
/// the recovered signer is not deep enough for the chunk (shallow).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ReceiptDepthError {
    /// The signature did not recover to a usable signer overlay. An all-zero
    /// signature (the structural failure signal) and any signature that does
    /// not recover both land here. Such a receipt is never trusted or relayed.
    #[error("receipt signature did not recover to a signer overlay")]
    MalformedSignature,

    /// The recovered signer is too shallow for the chunk:
    /// `PO(signer, chunk) < required`. The chunk never reached the responsible
    /// neighbourhood, so the custody claim is fraudulent (or, at best, useless
    /// for retrievability) and must be rejected.
    #[error("shallow receipt: signer proximity {observed} below required depth {required}")]
    Shallow {
        /// The required depth the signer had to reach.
        required: u8,
        /// The signer's actual proximity order to the chunk address.
        observed: u8,
    },
}

/// Recover the overlay of the node that signed a custody receipt.
///
/// The storer signs over the 32-byte chunk address with an EIP-191 personal
/// sign, exactly as the reference does; the nonce is NOT part of the signed
/// message, it is only an input to overlay derivation. The overlay is derived
/// from the recovered Ethereum address with the canonical
/// [`compute_overlay`](nectar_primitives::compute_overlay) formula
/// (`keccak256(eth || network_id_le || nonce)`).
///
/// Critically, this recovers the signer from the *signature*, NOT from the
/// off-wire `receipt.storer` field. On a multi-hop relay the handler sets
/// `receipt.storer` to the immediate downstream peer, which is several hops from
/// the real signer, so a depth check against `receipt.storer` would measure the
/// wrong peer. An all-zero (structural failure) signature, or any signature that
/// does not recover, yields [`ReceiptDepthError::MalformedSignature`].
pub fn recover_receipt_signer(
    receipt: &PushReceipt,
    address: &ChunkAddress,
    network_id: NetworkId,
) -> Result<OverlayAddress, ReceiptDepthError> {
    // The structural failure signal is an all-zero signature; never recover it.
    if receipt.signature.as_bytes() == [0u8; 65] {
        return Err(ReceiptDepthError::MalformedSignature);
    }

    // The signed message is the 32-byte chunk address only, matching the
    // reference producer; the nonce binds the overlay, not the signature.
    let eth = receipt
        .signature
        .recover_address_from_msg(address.as_bytes())
        .map_err(|_| ReceiptDepthError::MalformedSignature)?;

    Ok(compute_overlay(&eth, network_id, &receipt.nonce))
}

/// Slack subtracted from the locally observed depth before it becomes the floor.
///
/// Topology views drift: a peer at the genuine edge of the responsible
/// neighbourhood can sit one bin shallower than our own observed depth without
/// being a free-rider. Subtracting a small fixed tolerance from the local floor
/// (as the reference does with its own radius) avoids false-rejecting such an
/// honest, marginally-shallow receipt while still rejecting receipts that are
/// meaningfully outside the neighbourhood. The wire radius can only *raise* the
/// bar above this floor, never lower it.
pub const SHALLOW_RECEIPT_TOLERANCE: u8 = 1;

/// Derive the minimum depth a receipt signer must reach for a chunk.
///
/// The required depth is dynamic: hard-coding it would reject legitimately
/// shallow receipts in a small, young, or sparse neighbourhood, failing valid
/// uploads. The locally observed neighbourhood depth is the trusted authority
/// for how deep the responsible neighbourhood is, and it is the only input an
/// attacker cannot influence, so it sets the *floor* (minus a small
/// [`SHALLOW_RECEIPT_TOLERANCE`] for honest topology drift). The storer's
/// claimed `storage_radius` is attacker-controlled wire data: it is trusted only
/// to *raise* the bar, never to lower it. A storer that declares a deeper radius
/// than our observed depth is binding itself to a stricter custody claim, which
/// we honour; a storer that declares a shallower radius cannot thereby weaken the
/// locally trusted floor, so it cannot launder a shallow free-riding receipt past
/// the check by under-declaring (or zeroing) its radius.
///
/// Hence `required = max(local_depth - tolerance, wire_radius)`, mirroring the
/// reference, whose bar is `max(self_radius - tolerance, receipt.StorageRadius)`.
/// The depth check is a cheap first filter, not a cryptographic guarantee;
/// stake-binding and retrievability auditing (storage incentives, tracked in
/// #75) are the layer that makes forgery unprofitable.
#[must_use]
pub fn required_receipt_depth(local_depth: NeighborhoodDepth, wire_radius: StorageRadius) -> u8 {
    let floor = local_depth.get().saturating_sub(SHALLOW_RECEIPT_TOLERANCE);
    floor.max(wire_radius.get())
}

/// Verify a custody receipt is deep enough, returning the recovered signer.
///
/// Recovers the signer overlay from the signature (see
/// [`recover_receipt_signer`]), then checks
/// `PO(signer, chunk) >= required_receipt_depth(local_depth, receipt.storage_radius)`.
/// On success the signer overlay is returned so callers can attribute scoring to
/// the real signer; on failure the typed [`ReceiptDepthError`] tells the caller
/// whether the receipt was malformed or shallow.
pub fn verify_receipt_depth(
    receipt: &PushReceipt,
    address: &ChunkAddress,
    network_id: NetworkId,
    local_depth: NeighborhoodDepth,
) -> Result<OverlayAddress, ReceiptDepthError> {
    let signer = recover_receipt_signer(receipt, address, network_id)?;
    let required = required_receipt_depth(local_depth, receipt.storage_radius);
    let observed = address.proximity(&signer).get();
    if observed < required {
        return Err(ReceiptDepthError::Shallow { required, observed });
    }
    Ok(signer)
}

/// Trait for sending chunks to the Swarm network via PushSync.
///
/// Client nodes use this to upload chunks. A chunk and its postage stamp travel
/// together as a [`StampedChunk`]. Two modes are provided:
/// - `send_chunk_unchecked`: Trust the caller, no validation
/// - `send_chunk`: Validate stamp signature (but not batch validity)
#[async_trait::async_trait]
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait SwarmChunkSender: Send + Sync + 'static {
    /// Send a stamped chunk without any stamp validation.
    ///
    /// Trusts the caller has already validated the stamp. Use when:
    /// - Uploading freshly created chunks with known-good stamps
    /// - Internal operations where validation is redundant
    async fn send_chunk_unchecked(&self, chunk: StampedChunk) -> SwarmResult<PushReceipt>;

    /// Send a stamped chunk with stamp signature validation.
    ///
    /// Validates the stamp signature matches the chunk address, but does NOT
    /// check batch validity on-chain. Batch validity is the storer's concern.
    ///
    /// Returns `SwarmError::InvalidSignature` if the stamp doesn't match the chunk.
    async fn send_chunk(&self, chunk: StampedChunk) -> SwarmResult<PushReceipt>;
}

#[cfg(test)]
mod receipt_depth_tests {
    use alloy_signer::SignerSync;
    use alloy_signer_local::PrivateKeySigner;
    use nectar_primitives::Bin;

    use super::*;

    const NET: NetworkId = NetworkId::MAINNET;

    fn depth(n: u8) -> NeighborhoodDepth {
        NeighborhoodDepth::new(Bin::new(n).unwrap())
    }

    fn radius(n: u8) -> StorageRadius {
        StorageRadius::new(Bin::new(n).unwrap())
    }

    /// A chunk address with a controlled leading byte, so a signer overlay's
    /// proximity to it is easy to reason about.
    fn chunk_address(first_byte: u8) -> ChunkAddress {
        let mut bytes = [0u8; 32];
        bytes[0] = first_byte;
        ChunkAddress::new(bytes)
    }

    /// Sign a receipt over the 32-byte chunk address (the wire format), grinding
    /// the nonce until the signer's overlay shares at least `min_depth` leading
    /// bits with `address`. Returns the receipt and the signer overlay.
    fn signed_receipt(
        signer: &PrivateKeySigner,
        address: &ChunkAddress,
        min_depth: u8,
        storage_radius: StorageRadius,
    ) -> (PushReceipt, OverlayAddress) {
        let eth = signer.address();
        // The signature is over the address only and is independent of the
        // nonce, so sign once and grind the nonce purely for overlay depth.
        let signature = signer.sign_message_sync(address.as_bytes()).expect("sign");
        let mut counter = 0u64;
        loop {
            let mut nonce_bytes = [0u8; 32];
            nonce_bytes[..8].copy_from_slice(&counter.to_le_bytes());
            let nonce = Nonce::from(nonce_bytes);
            let overlay = compute_overlay(&eth, NET, &nonce);
            if address.proximity(&overlay).get() >= min_depth {
                return (
                    PushReceipt {
                        // The off-wire storer is deliberately a far address to
                        // prove the depth check never reads it.
                        storer: OverlayAddress::from([0xff; 32]),
                        signature,
                        nonce,
                        storage_radius,
                    },
                    overlay,
                );
            }
            counter += 1;
        }
    }

    #[test]
    fn required_depth_floors_on_local_and_wire_radius_can_only_raise() {
        // The locally observed depth (minus the tolerance) is the floor; the
        // attacker-controlled wire radius can only RAISE the bar above it.
        // Floor dominates when the claimed radius is shallower.
        assert_eq!(
            required_receipt_depth(depth(8), radius(4)),
            8 - SHALLOW_RECEIPT_TOLERANCE
        );
        // A shallow (or zero) wire radius cannot drop the bar below the floor.
        assert_eq!(
            required_receipt_depth(depth(8), radius(0)),
            8 - SHALLOW_RECEIPT_TOLERANCE
        );
        // A deeper claimed radius raises the bar above the floor.
        assert_eq!(required_receipt_depth(depth(4), radius(8)), 8);
        // With local depth 0 the floor saturates to 0; only a positive claimed
        // radius can raise it (it never collapses below the radius claim).
        assert_eq!(required_receipt_depth(depth(0), radius(0)), 0);
        assert_eq!(required_receipt_depth(depth(0), radius(31)), 31);
    }

    #[test]
    fn wire_radius_zero_cannot_lower_the_bar_below_the_local_floor() {
        // Regression: an attacker setting storage_radius == 0 must NOT collapse
        // the required depth to 0. A signer far from the chunk (shallow) is
        // rejected even though it declares radius 0, because the locally observed
        // depth floors the requirement.
        let signer = PrivateKeySigner::random();
        let address = chunk_address(0xff);
        // Grind only to depth 0 so the signer is realistically shallow, then
        // claim radius 0 to attempt to bypass the check.
        let (receipt, signer_overlay) = signed_receipt(&signer, &address, 0, radius(0));
        let observed = address.proximity(&signer_overlay).get();
        // Choose a local depth strictly deeper than the signer can reach.
        let local = observed + SHALLOW_RECEIPT_TOLERANCE + 1;
        let err = verify_receipt_depth(&receipt, &address, NET, depth(local))
            .expect_err("radius 0 does not bypass the local floor");
        assert!(matches!(err, ReceiptDepthError::Shallow { .. }));
    }

    #[test]
    fn recovers_signer_from_signature_not_storer_field() {
        let signer = PrivateKeySigner::random();
        let address = chunk_address(0xff);
        let (receipt, expected) = signed_receipt(&signer, &address, 8, radius(8));
        // The off-wire storer is [0xff; 32], not the signer overlay.
        assert_ne!(receipt.storer, expected);
        let recovered = recover_receipt_signer(&receipt, &address, NET).expect("recovers");
        assert_eq!(
            recovered, expected,
            "recovered from the signature, not storer"
        );
    }

    #[test]
    fn deep_enough_receipt_is_accepted() {
        let signer = PrivateKeySigner::random();
        let address = chunk_address(0xff);
        // Signer 8 bits deep; require 8 (local depth 8, wire radius 8).
        let (receipt, signer_overlay) = signed_receipt(&signer, &address, 8, radius(8));
        let got =
            verify_receipt_depth(&receipt, &address, NET, depth(8)).expect("deep enough accepted");
        assert_eq!(got, signer_overlay);
    }

    #[test]
    fn shallow_receipt_is_rejected() {
        let signer = PrivateKeySigner::random();
        let address = chunk_address(0xff);
        // Signer realistically shallow (grind only to depth 0); claim a shallow
        // radius so the wire radius does not raise the bar. The local floor is
        // what rejects it.
        let (receipt, signer_overlay) = signed_receipt(&signer, &address, 0, radius(0));
        let observed = address.proximity(&signer_overlay).get();
        // Local depth strictly deeper than the signer's reach (account for the
        // tolerance so the floor lands strictly above `observed`).
        let local = observed + SHALLOW_RECEIPT_TOLERANCE + 1;
        let required = required_receipt_depth(depth(local), radius(0));
        let err = verify_receipt_depth(&receipt, &address, NET, depth(local))
            .expect_err("shallow rejected");
        assert_eq!(err, ReceiptDepthError::Shallow { required, observed });
    }

    #[test]
    fn deeper_wire_radius_raises_the_bar_and_rejects_a_below_radius_signer() {
        // A storer that declares a deep radius binds itself to a stricter custody
        // claim. A signer below its own declared radius is rejected even when the
        // local floor alone would have accepted it.
        let signer = PrivateKeySigner::random();
        let address = chunk_address(0xff);
        // Signer ~4 bits deep, but claims radius 12.
        let (receipt, signer_overlay) = signed_receipt(&signer, &address, 4, radius(12));
        let observed = address.proximity(&signer_overlay).get();
        // Only proceed if the grind produced a signer below the claimed radius
        // (overwhelmingly the case: grinding to 12 is exponentially unlikely).
        if observed < 12 {
            let err = verify_receipt_depth(&receipt, &address, NET, depth(2))
                .expect_err("below-declared-radius signer rejected");
            assert_eq!(
                err,
                ReceiptDepthError::Shallow {
                    required: 12,
                    observed
                }
            );
        }
    }

    #[test]
    fn signature_is_over_the_32_byte_address_only_and_nonce_is_not_signed() {
        // Lock the wire format: the signature is produced over the 32-byte chunk
        // address with no nonce appended, matching the reference producer. A
        // receipt built that way (and only that way) must recover to the signer.
        let signer = PrivateKeySigner::random();
        let address = chunk_address(0xff);
        let eth = signer.address();
        let signature = signer.sign_message_sync(address.as_bytes()).expect("sign");

        // Find a nonce that lands the overlay deep enough to be accepted.
        let mut counter = 0u64;
        let (nonce, overlay) = loop {
            let mut nonce_bytes = [0u8; 32];
            nonce_bytes[..8].copy_from_slice(&counter.to_le_bytes());
            let nonce = Nonce::from(nonce_bytes);
            let overlay = compute_overlay(&eth, NET, &nonce);
            if address.proximity(&overlay).get() >= 8 {
                break (nonce, overlay);
            }
            counter += 1;
        };

        let receipt = PushReceipt {
            storer: OverlayAddress::from([0xff; 32]),
            signature,
            nonce,
            storage_radius: radius(8),
        };
        let recovered = recover_receipt_signer(&receipt, &address, NET).expect("recovers");
        assert_eq!(recovered, overlay, "32-byte-address signature recovers");
        let got = verify_receipt_depth(&receipt, &address, NET, depth(8))
            .expect("reference-format receipt accepted");
        assert_eq!(got, overlay);

        // A signature produced over (address || nonce) must NOT recover to this
        // signer for the 32-byte address scheme: a different message digest
        // yields an unrelated overlay that is shallow.
        let mut wrong_msg = [0u8; 64];
        wrong_msg[..32].copy_from_slice(address.as_bytes());
        wrong_msg[32..].copy_from_slice(nonce.as_slice());
        let wrong_sig = signer.sign_message_sync(&wrong_msg).expect("sign");
        let wrong_receipt = PushReceipt {
            signature: wrong_sig,
            ..receipt
        };
        let recovered_wrong =
            recover_receipt_signer(&wrong_receipt, &address, NET).expect("recovers to something");
        assert_ne!(
            recovered_wrong, overlay,
            "64-byte (address||nonce) signature recovers to a different overlay"
        );
    }

    #[test]
    fn all_zero_signature_is_malformed() {
        let address = chunk_address(0xff);
        let receipt = PushReceipt {
            storer: OverlayAddress::from([0x11; 32]),
            signature: Signature::from_raw(&[0u8; 65]).expect("zero signature parses"),
            nonce: Nonce::from([7u8; 32]),
            storage_radius: radius(8),
        };
        let err = verify_receipt_depth(&receipt, &address, NET, depth(8))
            .expect_err("zero signature rejected");
        assert_eq!(err, ReceiptDepthError::MalformedSignature);
    }

    #[test]
    fn wrong_message_recovers_to_a_different_overlay_and_is_shallow() {
        // A receipt whose signature was produced over a DIFFERENT message (a
        // different address) recovers to a different ethereum address, hence a
        // different overlay that is almost certainly shallow for this chunk.
        let signer = PrivateKeySigner::random();
        let real = chunk_address(0xff);
        let other = chunk_address(0x00);
        let (mut receipt, _) = signed_receipt(&signer, &other, 8, radius(31));
        // Reuse the signature against the real address: recovery succeeds (to
        // some overlay) but it is not deep for `real`.
        receipt.storage_radius = radius(31);
        let err = verify_receipt_depth(&receipt, &real, NET, depth(8))
            .expect_err("signature over a different address is shallow for this chunk");
        assert!(matches!(err, ReceiptDepthError::Shallow { .. }));
    }
}
