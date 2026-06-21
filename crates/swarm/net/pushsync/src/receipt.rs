//! The custody receipt, reconstructed and verified at the decode boundary.
//!
//! [`Receipt`] is the only receipt representation past the decode boundary. Its
//! sole constructor [`Receipt::reconstruct`] recovers the storer overlay from the
//! signature, so a receipt whose storer cannot be recovered never becomes a
//! [`Receipt`]; consumers always see a real, recovered `storer`.
//!
//! The storer is recovered, not read: it is not on the wire, and on a multi-hop
//! relay the delivering peer is several hops from the real storer. The storer
//! signs over the 32-byte chunk address only; the nonce binds the overlay, not
//! the signature.
//!
//! [`Receipt::verify_depth`] checks the storer is deep enough for the chunk. The
//! bar is `max(local_depth - tolerance, declared_radius)`, gated on a credible
//! neighbourhood view: an attacker controls the unsigned radius, so without a
//! credible local floor it returns [`DepthVerdict::Unverifiable`] rather than
//! trusting the radius alone.

use nectar_primitives::ChunkAddress;
use vertex_swarm_primitives::{
    NeighborhoodDepth, NetworkId, Nonce, OverlayAddress, OverlaySigner, StorageRadius,
    compute_overlay,
};

use crate::codec::WireReceipt;
use crate::error::PushsyncError;

/// Length in bytes of a well-formed recoverable signature.
const SIGNATURE_LEN: usize = 65;

/// Slack subtracted from the local depth before it becomes the floor, so an
/// honest peer one bin shallower than our observed depth (topology drift) is not
/// false-rejected.
pub const SHALLOW_RECEIPT_TOLERANCE: u8 = 1;

/// The storer is too shallow for the chunk: `PO(storer, chunk) < required`, so
/// the chunk never reached the responsible neighbourhood.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("shallow receipt: storer proximity {observed} below required depth {required}")]
pub struct ShallowReceipt {
    pub required: u8,
    /// The storer's proximity order to the chunk address.
    pub observed: u8,
}

impl From<&ShallowReceipt> for &'static str {
    /// Static metric label; the dynamic depths stay out of the label space.
    fn from(_: &ShallowReceipt) -> Self {
        "shallow_receipt"
    }
}

/// The outcome of [`Receipt::verify_depth`]. Three-valued to keep two failure
/// modes distinct: a [`Shallow`](Self::Shallow) verdict is a positive finding of
/// misbehaviour (penalise the responder); an
/// [`Unverifiable`](Self::Unverifiable) verdict is the absence of a finding (do
/// not penalise).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthVerdict {
    /// Deep enough against a credible local view.
    Verified,
    /// Provably too shallow against a credible local view; penalise the responder.
    Shallow(ShallowReceipt),
    /// Local view is not credible; custody depth cannot be judged. Treated as
    /// unconfirmed, not misbehaviour.
    Unverifiable,
}

/// A custody receipt whose storer has been recovered and verified.
///
/// Constructed only by [`Receipt::reconstruct`], so [`storer`](Self::storer) is
/// always a real overlay recovered from the signature. The signature and nonce
/// are retained so a forwarder can relay the receipt verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// The chunk this receipt acknowledges custody of.
    pub address: ChunkAddress,
    /// The real storer recovered from the signature, regardless of relayer.
    pub storer: OverlayAddress,
    /// The storer's signature over the 32-byte chunk address.
    pub signature: alloy_primitives::Signature,
    /// The nonce the storer used to derive its overlay.
    pub nonce: Nonce,
    /// The storer's declared storage radius at acceptance time.
    pub storage_radius: StorageRadius,
}

impl Receipt {
    /// Reconstruct and verify the storer of a wire receipt at the decode
    /// boundary.
    ///
    /// The storer overlay is derived from the recovered Ethereum address via
    /// [`compute_overlay`](vertex_swarm_primitives::compute_overlay)
    /// (`keccak256(eth || network_id_le || nonce)`).
    ///
    /// # Errors
    ///
    /// [`PushsyncError::MalformedReceiptSignature`] for an all-zero signature
    /// (the structural-failure signal) or any signature that fails recovery.
    pub fn reconstruct(wire: WireReceipt, network_id: NetworkId) -> Result<Self, PushsyncError> {
        // The all-zero signature is the structural-failure signal; never recover
        // it.
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
    /// `required = max(local_depth - tolerance, storage_radius)`. The local depth
    /// is the trusted authority (the one input an attacker cannot influence) and
    /// sets the floor, minus [`SHALLOW_RECEIPT_TOLERANCE`] for topology drift.
    /// The attacker-controlled declared `storage_radius` can only raise the bar,
    /// never lower it.
    ///
    /// When `neighbourhood_credible` is false the floor is meaningless (a fresh
    /// node's depth begins at zero), so the bar would collapse to the
    /// attacker-controlled radius; the check returns
    /// [`DepthVerdict::Unverifiable`] without reading the radius.
    ///
    /// This is a cheap first filter, not a cryptographic guarantee; storage
    /// incentives (#75) make forgery unprofitable.
    pub fn verify_depth(
        &self,
        local_depth: NeighborhoodDepth,
        neighbourhood_credible: bool,
    ) -> DepthVerdict {
        if !neighbourhood_credible {
            return DepthVerdict::Unverifiable;
        }
        let floor = local_depth.get().saturating_sub(SHALLOW_RECEIPT_TOLERANCE);
        let required = floor.max(self.storage_radius.get());
        let observed = self.address.proximity(&self.storer).get();
        if observed < required {
            return DepthVerdict::Shallow(ShallowReceipt { required, observed });
        }
        DepthVerdict::Verified
    }

    /// Mint a custody receipt for a chunk this node has taken into its reserve.
    ///
    /// Storer-side counterpart to [`reconstruct`](Self::reconstruct): produces a
    /// fresh receipt a forwarder will later reconstruct. The signer, `network_id`
    /// and `nonce` come from the [`OverlaySigner`] identity; `storage_radius`
    /// tracks the reserve and stays an explicit argument.
    ///
    /// The `storer` is derived by recovering the Ethereum address from the freshly
    /// minted signature and applying [`compute_overlay`], exactly as the read side
    /// does, so the receipt is self-consistent under recovery and verified by
    /// construction.
    pub fn sign(
        identity: &impl OverlaySigner,
        address: ChunkAddress,
        storage_radius: StorageRadius,
    ) -> Result<Self, PushsyncError> {
        let nonce = identity.nonce();
        let signature = identity
            .sign_message_sync(address.as_bytes())
            .map_err(|_| PushsyncError::MalformedReceiptSignature)?;
        // Derive the overlay the same way `reconstruct` does, so a forwarder
        // recovers exactly this overlay.
        let eth = signature
            .recover_address_from_msg(address.as_bytes())
            .map_err(|_| PushsyncError::MalformedReceiptSignature)?;
        let storer = compute_overlay(&eth, identity.network_id(), &nonce);
        Ok(Self {
            address,
            storer,
            signature,
            nonce,
            storage_radius,
        })
    }

    /// Reproduce the wire receipt for verbatim relay.
    ///
    /// The recovered `storer` is not on the wire, so this emits only the bytes the
    /// storer signed.
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
    use alloy_primitives::{Address, B256, ChainId, Signature};
    use alloy_signer_local::PrivateKeySigner;
    use nectar_primitives::Bin;
    use vertex_swarm_primitives::SignerSync;

    use super::*;

    const NET: NetworkId = NetworkId::MAINNET;

    /// Test-only [`OverlaySigner`] so the tests mint through [`Receipt::sign`].
    struct TestIdentity {
        signer: PrivateKeySigner,
        nonce: Nonce,
    }

    impl TestIdentity {
        fn new(signer: PrivateKeySigner, nonce: Nonce) -> Self {
            Self { signer, nonce }
        }
    }

    impl SignerSync for TestIdentity {
        fn sign_hash_sync(&self, hash: &B256) -> alloy_signer::Result<Signature> {
            self.signer.sign_hash_sync(hash)
        }

        fn chain_id_sync(&self) -> Option<ChainId> {
            self.signer.chain_id_sync()
        }
    }

    impl OverlaySigner for TestIdentity {
        fn address(&self) -> Address {
            self.signer.address()
        }

        fn network_id(&self) -> NetworkId {
            NET
        }

        fn nonce(&self) -> Nonce {
            self.nonce
        }
    }

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

    /// Grind the nonce until the storer overlay sits at least `min_depth` bits
    /// deep relative to `address`, then mint via [`Receipt::sign`] and reduce to
    /// wire form. The grind only constructs a test storer at a known proximity.
    fn wire(
        signer: &PrivateKeySigner,
        address: &ChunkAddress,
        min_depth: u8,
        storage_radius: StorageRadius,
    ) -> (WireReceipt, OverlayAddress) {
        let eth = signer.address();
        let mut counter = 0u64;
        loop {
            let mut nonce_bytes = [0u8; 32];
            nonce_bytes[..8].copy_from_slice(&counter.to_le_bytes());
            let nonce = Nonce::from(nonce_bytes);
            let overlay = compute_overlay(&eth, NET, &nonce);
            if address.proximity(&overlay).get() >= min_depth {
                let identity = TestIdentity::new(signer.clone(), nonce);
                let receipt =
                    Receipt::sign(&identity, *address, storage_radius).expect("sign receipt");
                return (receipt.to_wire(), overlay);
            }
            counter += 1;
        }
    }

    #[test]
    fn sign_mints_a_receipt_that_reconstructs_to_the_same_storer() {
        // Mint and recovery are inverses.
        let signer = PrivateKeySigner::random();
        let address = chunk_address(0x42);
        let nonce = Nonce::from([3u8; 32]);
        let eth = signer.address();
        let identity = TestIdentity::new(signer, nonce);
        let minted = Receipt::sign(&identity, address, radius(7)).expect("mints a receipt");
        let expected = compute_overlay(&eth, NET, &nonce);
        assert_eq!(minted.storer, expected, "storer derived from the signature");
        assert_eq!(minted.address, address);
        assert_eq!(minted.nonce, nonce);
        assert_eq!(minted.storage_radius, radius(7));

        // Round-trip through the decode boundary is field-identical.
        let reconstructed =
            Receipt::reconstruct(minted.to_wire(), NET).expect("reconstructs the minted receipt");
        assert_eq!(reconstructed, minted);
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
        assert_eq!(receipt.verify_depth(depth(8), true), DepthVerdict::Verified);
    }

    #[test]
    fn shallow_receipt_is_rejected_and_radius_zero_cannot_lower_the_bar() {
        let signer = PrivateKeySigner::random();
        let address = chunk_address(0xff);
        // Shallow storer (depth 0) declaring radius 0 cannot bypass the local
        // floor under a credible view.
        let (raw, storer) = wire(&signer, &address, 0, radius(0));
        let observed = address.proximity(&storer).get();
        let local = observed + SHALLOW_RECEIPT_TOLERANCE + 1;
        let receipt = Receipt::reconstruct(raw, NET).expect("reconstructs");
        let DepthVerdict::Shallow(err) = receipt.verify_depth(depth(local), true) else {
            panic!("radius 0 does not bypass the local floor");
        };
        assert_eq!(err.observed, observed);
        assert!(err.required > observed);
    }

    #[test]
    fn non_credible_view_returns_unverifiable_even_when_the_floor_collapses() {
        // Regression for #316: under a non-credible view the worst case
        // (local_depth 0, radius 0, storer at depth 0) is Unverifiable, not
        // Verified.
        let signer = PrivateKeySigner::random();
        let address = chunk_address(0xff);
        let (raw, _) = wire(&signer, &address, 0, radius(0));
        let receipt = Receipt::reconstruct(raw, NET).expect("reconstructs");
        assert_eq!(
            receipt.verify_depth(depth(0), false),
            DepthVerdict::Unverifiable,
            "a shallow receipt is unverifiable, not accepted, under a non-credible view"
        );
    }

    #[test]
    fn declared_radius_can_only_raise_the_bar() {
        let signer = PrivateKeySigner::random();
        let address = chunk_address(0xff);
        // A ~4-bit-deep storer declaring radius 12 binds a stricter claim and is
        // rejected even where the local floor alone would accept it.
        let (raw, storer) = wire(&signer, &address, 4, radius(12));
        let observed = address.proximity(&storer).get();
        assert!(
            observed < 12,
            "constructed storer sits below the declared radius"
        );
        let receipt = Receipt::reconstruct(raw, NET).expect("reconstructs");
        assert_eq!(
            receipt.verify_depth(depth(2), true),
            DepthVerdict::Shallow(ShallowReceipt {
                required: 12,
                observed
            }),
            "below-declared-radius storer rejected"
        );
    }
}
