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
/// The storer signs over `(chunk_address || nonce)` with an EIP-191 personal
/// sign, exactly as the reference does; the overlay is then derived from the
/// recovered Ethereum address with the canonical
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

    let mut message = [0u8; 64];
    message[..32].copy_from_slice(address.as_bytes());
    message[32..].copy_from_slice(receipt.nonce.as_slice());

    let eth = receipt
        .signature
        .recover_address_from_msg(message.as_slice())
        .map_err(|_| ReceiptDepthError::MalformedSignature)?;

    Ok(compute_overlay(&eth, network_id, &receipt.nonce))
}

/// Derive the minimum depth a receipt signer must reach for a chunk.
///
/// The required depth is dynamic: hard-coding it would reject legitimately
/// shallow receipts in a small, young, or sparse neighbourhood, failing valid
/// uploads. The locally observed neighbourhood depth is the trusted authority
/// for how deep the responsible neighbourhood is; we never require *more* than
/// what we can observe (a stale-high local estimate must not false-reject a
/// correct receipt). The storer's claimed `storage_radius` is trusted only to
/// *lower* the bar, never to raise it: a storer at the edge of a genuinely
/// sparse neighbourhood may legitimately carry a shallower radius than our
/// global depth estimate, and requiring more than its self-declared
/// responsibility would false-reject it.
///
/// Hence `required = min(local_depth, wire_radius)`. A malicious storer that
/// lowers `storage_radius` to weaken the check thereby self-declares
/// non-responsibility for the chunk, which is exactly the property a custody
/// receipt is supposed to assert; the soft filter still rejects the
/// zero-cost case (a forwarder signing with its existing wrong-neighbourhood
/// overlay) and the grind cost rises to the neighbourhood size. The depth check
/// is a cheap first filter, not a cryptographic guarantee; stake-binding and
/// retrievability auditing (storage incentives, tracked in #75) are the layer
/// that makes forgery unprofitable.
#[must_use]
pub fn required_receipt_depth(local_depth: NeighborhoodDepth, wire_radius: StorageRadius) -> u8 {
    local_depth.get().min(wire_radius.get())
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

    /// Sign a receipt over `(address || nonce)`, grinding the nonce until the
    /// signer's overlay shares at least `min_depth` leading bits with `address`.
    /// Returns the receipt and the signer overlay.
    fn signed_receipt(
        signer: &PrivateKeySigner,
        address: &ChunkAddress,
        min_depth: u8,
        storage_radius: StorageRadius,
    ) -> (PushReceipt, OverlayAddress) {
        let eth = signer.address();
        let mut counter = 0u64;
        loop {
            let mut nonce_bytes = [0u8; 32];
            nonce_bytes[..8].copy_from_slice(&counter.to_le_bytes());
            let nonce = Nonce::from(nonce_bytes);
            let overlay = compute_overlay(&eth, NET, &nonce);
            if address.proximity(&overlay).get() >= min_depth {
                let mut message = [0u8; 64];
                message[..32].copy_from_slice(address.as_bytes());
                message[32..].copy_from_slice(nonce.as_slice());
                let signature = signer.sign_message_sync(&message).expect("sign");
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
    fn required_depth_is_min_of_local_and_wire() {
        // The required depth never exceeds the locally observed depth and never
        // exceeds the storer's self-declared radius.
        assert_eq!(required_receipt_depth(depth(8), radius(4)), 4);
        assert_eq!(required_receipt_depth(depth(4), radius(8)), 4);
        assert_eq!(required_receipt_depth(depth(0), radius(31)), 0);
        assert_eq!(required_receipt_depth(depth(12), radius(12)), 12);
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
        // Signer at most ~2 bits deep (grind only to depth 0, so realistically
        // shallow); require 8. We pin the observed depth from the receipt.
        let (receipt, signer_overlay) = signed_receipt(&signer, &address, 0, radius(31));
        let observed = address.proximity(&signer_overlay).get();
        // Pick a required depth strictly greater than the signer's reach.
        let required = observed + 1;
        let err = verify_receipt_depth(&receipt, &address, NET, depth(required))
            .expect_err("shallow rejected");
        assert_eq!(err, ReceiptDepthError::Shallow { required, observed });
    }

    #[test]
    fn wire_radius_lowers_the_bar_for_sparse_neighbourhoods() {
        // A legitimately sparse neighbourhood: the storer's own radius is
        // shallow (2) even though our global depth estimate is deeper (10). The
        // required depth is bounded by the storer's claim, so a correct receipt
        // for a sparse neighbourhood is NOT false-rejected.
        let signer = PrivateKeySigner::random();
        let address = chunk_address(0xff);
        let (receipt, _) = signed_receipt(&signer, &address, 2, radius(2));
        verify_receipt_depth(&receipt, &address, NET, depth(10))
            .expect("sparse-but-correct receipt accepted");
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
