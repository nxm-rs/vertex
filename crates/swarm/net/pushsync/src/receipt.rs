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
//! the type so consumers call
//! `receipt.verify_depth(local_depth, neighbourhood_credible)` directly.
//!
//! The check is gated on a credible neighbourhood view. The local depth is an
//! atomic that begins at zero on a fresh, sparse, or just-restarted node, and
//! the receipt's declared radius is unsigned wire data the responder controls.
//! With a low local floor the radius would become the sole bar, which a
//! responder can set to zero, so a shallow receipt would be wrongly accepted. A
//! caller therefore passes whether its neighbourhood view is credible (its
//! topology has saturated, so the observed depth reflects a real boundary); when
//! it is not, the check returns [`DepthVerdict::Unverifiable`] rather than
//! leaning on an attacker-controlled field it cannot anchor.
//!
//! # Future wire format
//!
//! When the wire format is later refined to carry a self-authenticating storer
//! (tracked under a separate fork-gated proposal), only [`Receipt::reconstruct`]
//! changes: the [`Receipt`] type and every consumer stay put. The recovery has no
//! topology or database dependency (only the signature, the network id, and
//! `compute_overlay`); a separate issue tracks lifting it into nectar as a
//! reusable primitive.

use alloy_signer::SignerSync;
use nectar_primitives::ChunkAddress;
use vertex_swarm_primitives::{
    NeighborhoodDepth, NetworkId, Nonce, OverlayAddress, StorageRadius, compute_overlay,
};

use crate::codec::WireReceipt;
use crate::error::PushsyncError;

/// A node identity that can mint its own custody receipt.
///
/// [`Receipt::sign`] needs three things from the minting node, and they are not
/// three independent parameters: they are all facets of the node's single
/// identity. The signing key proves custody, and the `network_id` and `nonce`
/// are the two inputs that, together with the recovered signing address, derive
/// the storer overlay via [`compute_overlay`] (the same derivation a forwarder
/// replays in [`Receipt::reconstruct`]). Threading them as loose arguments
/// duplicated state the identity already owns, so they are bundled behind this
/// one handle: the caller passes its identity and the receipt path reads the
/// signer and the overlay-derivation inputs from it.
///
/// This trait is deliberately minimal and lives in the pushsync layer rather
/// than referencing the node's full identity type: pushsync sits below the node
/// and cannot depend on it. A node-layer identity implements this trait (or a
/// node-owned capability bundle does), so the richer identity remains the single
/// source of truth without leaking node concerns into this crate.
pub trait ReceiptSigner {
    /// The synchronous signing key. Erased trait objects
    /// (`dyn SignerSync + Send + Sync`) satisfy this via `alloy-signer`'s blanket
    /// impls, so a node can plug in its key without this crate being generic over
    /// the concrete signer.
    type Signer: alloy_signer::SignerSync + ?Sized;

    /// The receipt signing key, signing over the 32-byte chunk address.
    fn signer(&self) -> &Self::Signer;

    /// The network id, one of the two overlay-derivation inputs.
    fn network_id(&self) -> NetworkId;

    /// The node's identity nonce, the other overlay-derivation input. It binds
    /// the storer overlay but is not part of the signed message.
    fn nonce(&self) -> Nonce;
}

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

/// The outcome of [`Receipt::verify_depth`].
///
/// The check is three-valued because there are two distinct failure modes that
/// must not be conflated. A [`Shallow`](Self::Shallow) verdict is a positive
/// finding of misbehaviour: against a credible local view the storer is provably
/// too shallow, so the responder is penalised. An
/// [`Unverifiable`](Self::Unverifiable) verdict is the absence of a finding: the
/// local view is not credible enough to judge custody depth (a fresh, sparse, or
/// just-restarted node before its neighbourhood saturates), so the receipt is
/// neither trusted nor blamed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthVerdict {
    /// The storer is deep enough for the chunk against a credible local view.
    Verified,
    /// The storer is provably too shallow against a credible local view; the
    /// responder must be penalised.
    Shallow(ShallowReceipt),
    /// The local neighbourhood view is not credible, so custody depth cannot be
    /// judged. The receipt is treated as unconfirmed, not as misbehaviour: the
    /// responder is not penalised.
    Unverifiable,
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
    /// The floor is only meaningful when `neighbourhood_credible` is true. The
    /// local depth is an atomic that begins at zero before the neighbourhood
    /// saturates, and the declared radius is unsigned wire data the responder
    /// controls. With a non-credible view the floor would collapse to the
    /// attacker-controlled radius (which the responder can set to zero), so there
    /// is no bar to anchor: the check returns [`DepthVerdict::Unverifiable`]
    /// WITHOUT reading the radius. The caller treats that as unconfirmed custody,
    /// not as misbehaviour.
    ///
    /// The depth check is a cheap first filter, not a cryptographic guarantee;
    /// stake-binding and retrievability auditing (storage incentives, tracked in
    /// #75) are the layer that makes forgery unprofitable.
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
    /// This is the storer-side counterpart to [`reconstruct`](Self::reconstruct):
    /// where reconstruct recovers a relayed receipt's storer from the signature,
    /// `sign` produces a fresh receipt that a forwarder will later reconstruct.
    /// Everything the minting node contributes comes from its [`ReceiptSigner`]
    /// identity: it signs over the 32-byte chunk address only (matching what
    /// `reconstruct` recovers from), and its `network_id` and `nonce` are the two
    /// overlay-derivation inputs. The `nonce` binds the storer overlay but is not
    /// part of the signed message. `storage_radius` is the only per-receipt input
    /// the identity does not own (it tracks the reserve), so it stays an explicit
    /// argument.
    ///
    /// Taking the identity as one handle, rather than the signer plus loose
    /// `network_id`/`nonce` parameters, keeps the overlay-derivation inputs with
    /// the key that owns them: the storer overlay is a property of the identity,
    /// so the three are never passed inconsistently.
    ///
    /// The receipt round-trips through [`reconstruct`](Self::reconstruct): the
    /// `storer` is derived by recovering the signer's Ethereum address from the
    /// freshly minted signature and applying [`compute_overlay`] with the
    /// identity's nonce, exactly as the read side does. Deriving it from the
    /// recovered address (rather than asking the signer for its address)
    /// guarantees the minted receipt is self-consistent under recovery, so the
    /// returned [`Receipt`] is fully verified by construction.
    pub fn sign(
        identity: &impl ReceiptSigner,
        address: ChunkAddress,
        storage_radius: StorageRadius,
    ) -> Result<Self, PushsyncError> {
        let nonce = identity.nonce();
        let signature = identity
            .signer()
            .sign_message_sync(address.as_bytes())
            .map_err(|_| PushsyncError::MalformedReceiptSignature)?;
        // Derive the storer overlay the same way `reconstruct` does on the read
        // side: recover the Ethereum address from the signature over the chunk
        // address, then combine it with the identity's network id and nonce. A
        // forwarder reconstructing this receipt recovers exactly this overlay.
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
    use alloy_signer_local::PrivateKeySigner;
    use nectar_primitives::Bin;

    use super::*;

    const NET: NetworkId = NetworkId::MAINNET;

    /// A test-only [`ReceiptSigner`]: a real signing key plus the identity nonce
    /// and network id, bundled exactly as a node identity bundles them. Lets the
    /// receipt tests mint through the production [`Receipt::sign`] path.
    struct TestIdentity {
        signer: PrivateKeySigner,
        nonce: Nonce,
    }

    impl TestIdentity {
        fn new(signer: PrivateKeySigner, nonce: Nonce) -> Self {
            Self { signer, nonce }
        }
    }

    impl ReceiptSigner for TestIdentity {
        type Signer = PrivateKeySigner;

        fn signer(&self) -> &Self::Signer {
            &self.signer
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

    /// Grind the node's identity nonce until the storer overlay sits at least
    /// `min_depth` bits deep relative to `address`, then mint the receipt with the
    /// production [`Receipt::sign`] path and reduce it to its wire form.
    ///
    /// A real node has a fixed identity nonce; the grind here only constructs a
    /// *test* storer at a chosen depth so the depth-policy tests have a known
    /// proximity. The receipt itself is minted exactly as production does.
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
        // The storer-side mint and the forwarder-side recovery are inverses: a
        // receipt signed with the node's identity nonce reconstructs to the
        // overlay that nonce plus the signing key derive.
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

        // Round-trip through the wire and the forwarder-side decode boundary: the
        // reconstructed receipt is byte- and field-identical to the minted one.
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
        // The storer is shallow (grind only to depth 0) and declares radius 0 to
        // try to bypass the check; against a credible local view the local floor
        // (depth 12) rejects it anyway.
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
        // Regression for #316: with a non-credible neighbourhood view the local
        // floor is meaningless and the unsigned radius must not become the sole
        // bar. Even at the worst case (local_depth == 0, storage_radius == 0, a
        // storer ground to depth 0) the verdict is Unverifiable, never Verified.
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
        // A storer ~4 bits deep declaring radius 12 binds itself to a stricter
        // claim; it is rejected even where the local floor alone would accept it.
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
