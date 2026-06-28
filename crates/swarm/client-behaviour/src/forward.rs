//! The forwarder seam: relay a retrieval or a pushsync to a closer peer.
//!
//! Inbound serving is handler-inline: each inbound retrieval or pushsync request
//! becomes one self-contained future, with the substream itself as the
//! correlation (mirroring the outbound model). When the local cache cannot
//! answer a retrieval, or for every pushsync, the handler hands off to a
//! [`Forwarder`] that relays to a closer peer and returns the result.
//!
//! This module holds the accounting-free trait side: the [`Forwarder`] contract,
//! the [`StubForwarder`] that never relays, the [`ForwardedChunk`] /
//! [`ForwardedReceipt`] carriers, and the [`closer_candidates`] loop-prevention
//! selector. The concrete network forwarder couples to accounting and the
//! outbound client handle, so it lives next to its wiring in the node crate.
//!
//! # Upstream credit is deferred to the wire write
//!
//! The two legs are not both committed inside the forwarder. The downstream
//! `receive` leg is genuinely complete the moment a verified chunk/receipt is in
//! hand, so it is applied there. The upstream `provide` leg (the requester or
//! pusher paying us) is *not* applied there: it is returned to the handler as a
//! boxed [`CommitOnWrite`] and committed only after the chunk or receipt is
//! successfully written back to the requester's substream. If that wire write
//! fails, the handler drops the action, releasing the reservation, so the
//! requester is never charged for a delivery it did not receive.

use futures::future::BoxFuture;
use nectar_primitives::{AnyChunk, ChunkAddress};
use vertex_swarm_api::{CommitOnWrite, SwarmTopologyRouting};
use vertex_swarm_net_pushsync::Receipt;
use vertex_swarm_primitives::{OverlayAddress, Stamp, StampedChunk};

/// Maximum number of closer peers a node tries, in order, for a single inbound
/// request before giving up.
///
/// This is a per-node fan-out cap, **not** a per-request hop/TTL counter: it
/// bounds how many downstream candidates one node retries for one inbound
/// request, not the length of the overall A->B->C->... relay chain. Termination
/// of the chain comes from the strictly-closer rule (every hop must hand the
/// request to a peer strictly closer to the target by XOR distance than both the
/// requester and that node), which makes proximity monotonically increase toward
/// the target and is bounded by the address width, so no per-request hop counter
/// or visited set is needed.
pub(crate) const MAX_FORWARD_CANDIDATES: usize = 3;

/// Why a forward could not complete.
///
/// The reason is intentionally coarse: the handler only needs to know the
/// forward did not produce a chunk or receipt so it can reset the inbound
/// substream. A real forwarder carries richer diagnostics for its own metrics,
/// but the inbound serving path treats every failure as a reset.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ForwardError {
    /// No peer strictly closer to the target than both the requester and this
    /// node is available to relay to. Covers both the no-candidate case and the
    /// loop-prevention case (every candidate would forward sideways or backwards
    /// in distance).
    #[error("no closer peer to forward to")]
    NoCloserPeer,

    /// Every strictly-closer candidate we tried failed to answer.
    #[error("all closer peers failed to relay")]
    AllPeersFailed,

    /// The upstream leg returned a chunk that does not answer the requested
    /// address. Never relayed.
    #[error("upstream relay returned unverified data")]
    UnverifiedRelay,

    /// The upstream leg returned a custody receipt whose recovered storer is not
    /// deep enough for the chunk (`PO(storer, chunk) < required`). A forwarder
    /// must never launder a shallow receipt upstream: it is dropped here and the
    /// downstream peer is scored adversely. A malformed receipt does not reach
    /// here; it is rejected at the downstream decode boundary and surfaces as a
    /// push failure ([`AllPeersFailed`](Self::AllPeersFailed)).
    #[error("upstream relay returned a shallow custody receipt")]
    ShallowReceipt,

    /// The upstream leg returned a custody receipt that cannot be judged because
    /// the local neighbourhood view is not credible (the neighbourhood has not
    /// saturated yet). The receipt is dropped without relaying it, but the
    /// downstream peer is NOT penalised: the receipt may be honest, the local
    /// node just lacks a trustworthy depth to anchor the check against. The relay
    /// continues to the next candidate. Distinct from
    /// [`ShallowReceipt`](Self::ShallowReceipt), which is a proven, penalised
    /// finding of misbehaviour.
    #[error("upstream relay returned an unverifiable custody receipt")]
    UnverifiableReceipt,

    /// Accounting refused one of the two legs (over the disconnect threshold),
    /// so the relay was not attempted. Any reservation already taken is released
    /// on drop.
    #[error("accounting refused the relay")]
    AccountingRefused,
}

/// Relays a retrieval or a pushsync to a closer peer on behalf of an inbound
/// request.
///
/// `exclude` is the requester or pusher, passed so the forwarder never relays
/// back to the peer that asked (loop prevention) and so it can account the
/// inbound leg against it. The returned futures are `'static`, boxed, and `Send`
/// so the handler can hold them in its inbound set: a libp2p `ConnectionHandler`
/// is `Send` on both native and wasm (the browser `Stream` is itself `Send`), so
/// the inbound serving futures are `Send` too.
pub trait Forwarder: Send + Sync {
    /// Retrieve `address` from a closer peer, excluding `exclude`.
    ///
    /// On success the downstream `receive` leg is already committed (we did
    /// receive the chunk), and the un-applied upstream `provide` action is
    /// returned alongside the chunk: the handler commits it only after the chunk
    /// is written back to the requester, and drops it (releasing the
    /// reservation) if that wire write fails.
    fn retrieve(
        &self,
        address: ChunkAddress,
        exclude: OverlayAddress,
    ) -> BoxFuture<'static, Result<ForwardedChunk, ForwardError>>;

    /// Push `chunk` to a closer peer, excluding `exclude`, returning the
    /// storer's receipt to relay verbatim.
    ///
    /// The upstream `provide` action is returned un-applied for the same
    /// deferred-commit reason as [`retrieve`](Self::retrieve).
    fn push(
        &self,
        chunk: StampedChunk,
        exclude: OverlayAddress,
    ) -> BoxFuture<'static, Result<ForwardedReceipt, ForwardError>>;
}

/// A relayed chunk together with the un-applied upstream credit.
///
/// The forwarder hands this to the handler, which writes `chunk` back to the
/// requester and only then commits `provide` (or drops it on a wire-write
/// failure, releasing the reservation).
///
/// The stamp is optional: a storer answers a retrieval with the chunk bytes and
/// may omit the stamp from the delivery. A stampless relayed chunk is served
/// onward stampless (the requester validates it against its address, which is
/// stamp-independent) and is not cached, since the cache value requires a stamp.
pub struct ForwardedChunk {
    /// The verified chunk to write back to the requester.
    pub chunk: AnyChunk,
    /// The stamp the downstream peer attached, if any.
    pub stamp: Option<Stamp>,
    /// The un-applied upstream credit; commit after a successful wire write.
    pub provide: Box<dyn CommitOnWrite>,
}

impl std::fmt::Debug for ForwardedChunk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForwardedChunk")
            .field("chunk", &self.chunk)
            .field("stamp", &self.stamp)
            .finish_non_exhaustive()
    }
}

/// A relayed receipt together with the un-applied upstream credit.
pub struct ForwardedReceipt {
    /// The storer's verified receipt to relay verbatim to the pusher. Its storer
    /// was recovered at the downstream decode boundary, so a forwarder only
    /// checks the depth policy before relaying.
    pub receipt: Receipt,
    /// The un-applied upstream credit; commit after a successful wire write.
    pub provide: Box<dyn CommitOnWrite>,
}

impl std::fmt::Debug for ForwardedReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForwardedReceipt")
            .field("receipt", &self.receipt)
            .finish_non_exhaustive()
    }
}

/// A forwarder that never relays: every relay fails with
/// [`ForwardError::NoCloserPeer`].
///
/// A cache miss therefore resets the inbound retrieval substream and every
/// inbound pushsync resets too, which is the correct behaviour for a node that
/// holds no reserve, takes no custody, and does not participate as a relay.
pub struct StubForwarder;

impl Forwarder for StubForwarder {
    fn retrieve(
        &self,
        _address: ChunkAddress,
        _exclude: OverlayAddress,
    ) -> BoxFuture<'static, Result<ForwardedChunk, ForwardError>> {
        Box::pin(async { Err(ForwardError::NoCloserPeer) })
    }

    fn push(
        &self,
        _chunk: StampedChunk,
        _exclude: OverlayAddress,
    ) -> BoxFuture<'static, Result<ForwardedReceipt, ForwardError>> {
        Box::pin(async { Err(ForwardError::NoCloserPeer) })
    }
}

/// Select the peers strictly closer to `target` than both `requester` and
/// `local`, excluding the requester and `local`, in proximity order, capped at
/// [`MAX_FORWARD_CANDIDATES`].
///
/// This is the loop-prevention core. A candidate is kept only when it is
/// strictly closer to the target than the requester (so the request never routes
/// sideways or backwards and can never cycle) **and** strictly closer than this
/// node (so a node already in the chunk's neighbourhood does not relay sideways
/// to an equally deep peer, avoiding the capped-PO plateau where all deep peers
/// compare equal). Using full XOR distance rather than capped proximity also lets
/// the strict comparison distinguish peers inside the deepest band.
pub fn closer_candidates(
    topology: &impl SwarmTopologyRouting,
    target: &ChunkAddress,
    requester: OverlayAddress,
    local: OverlayAddress,
) -> Vec<OverlayAddress> {
    topology
        .closest_to(target, MAX_FORWARD_CANDIDATES * 2)
        .into_iter()
        .filter(|peer| *peer != requester && *peer != local)
        // `target.closer(peer, other)` is true iff `peer` is strictly closer to
        // `target` than `other` by full XOR distance. The candidate must beat
        // both the requester (loop prevention) and this node (the
        // self-relative "closer than me" gate).
        .filter(|peer| target.closer(peer, &requester) && target.closer(peer, &local))
        .take(MAX_FORWARD_CANDIDATES)
        .collect()
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{B256, Signature};
    use nectar_postage::Stamp;
    use nectar_primitives::{AnyChunk, ContentChunk};
    use vertex_swarm_primitives::StampedChunk;
    use vertex_swarm_test_utils::MockTopology;

    use super::*;

    /// A stamped content chunk and its content-derived address.
    fn stamped() -> StampedChunk {
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        let stamp = Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig);
        let chunk: AnyChunk = ContentChunk::new(&b"forwarded payload"[..])
            .expect("valid content chunk")
            .into();
        StampedChunk::new(chunk, stamp)
    }

    /// Build an overlay sharing `leading_bits` leading bits with `address`, so
    /// its proximity to the address is exactly `leading_bits` (the next bit is
    /// flipped). Used to place a peer at a controlled distance from the target.
    fn overlay_at_proximity(address: &ChunkAddress, leading_bits: usize) -> OverlayAddress {
        let mut bytes = address.0.0;
        // Flip the bit immediately after the shared prefix so the proximity is
        // exactly `leading_bits`: the first differing bit caps proximity.
        let byte = leading_bits / 8;
        let bit = 7 - (leading_bits % 8);
        if let Some(b) = bytes.get_mut(byte) {
            *b ^= 1 << bit;
        }
        OverlayAddress::from(bytes)
    }

    #[test]
    fn closer_candidates_keeps_only_strictly_closer_peers() {
        let address = *stamped().address();
        // Requester shares 4 leading bits with the target.
        let requester = overlay_at_proximity(&address, 4);
        let closer = overlay_at_proximity(&address, 10);
        let sideways = overlay_at_proximity(&address, 4);
        let farther = overlay_at_proximity(&address, 1);
        let local = OverlayAddress::from([0xee; 32]);

        let topo = MockTopology::default().with_closest(vec![closer, sideways, farther]);
        let got = closer_candidates(&topo, &address, requester, local);
        assert_eq!(got, vec![closer], "only the strictly-closer peer survives");
    }

    #[test]
    fn closer_candidates_excludes_requester_and_local() {
        let address = *stamped().address();
        let requester = overlay_at_proximity(&address, 4);
        // Local is farther from the target than the candidate, so the
        // self-relative gate does not reject the candidate; this isolates the
        // exclusion behaviour (local must be dropped because it is us, not
        // because of the closer-than-me gate).
        let local = overlay_at_proximity(&address, 2);
        let closer = overlay_at_proximity(&address, 10);

        // The topology returns local and requester among the closest; both must
        // be filtered out as self/requester.
        let topo = MockTopology::default().with_closest(vec![local, closer, requester]);
        let got = closer_candidates(&topo, &address, requester, local);
        assert_eq!(got, vec![closer]);
    }

    #[test]
    fn closer_candidates_drops_peers_farther_than_local() {
        // The self-relative gate: a node already deeper in the target's
        // neighbourhood than a candidate must not relay sideways/back to that
        // candidate, even when the candidate is still closer than the requester.
        let address = *stamped().address();
        let requester = overlay_at_proximity(&address, 2);
        // We (local) share 12 bits with the target.
        let local = overlay_at_proximity(&address, 12);
        // The candidate is closer than the requester (8 > 2) but farther than us
        // (8 < 12), so it must be rejected.
        let farther_than_local = overlay_at_proximity(&address, 8);
        // A candidate deeper than us survives.
        let deeper = overlay_at_proximity(&address, 20);

        let topo = MockTopology::default().with_closest(vec![deeper, farther_than_local]);
        let got = closer_candidates(&topo, &address, requester, local);
        assert_eq!(
            got,
            vec![deeper],
            "only a peer strictly closer than this node survives"
        );
    }

    #[test]
    fn closer_candidates_empty_when_nothing_is_closer() {
        let address = *stamped().address();
        let requester = overlay_at_proximity(&address, 12);
        let local = OverlayAddress::from([0xee; 32]);
        let sideways = overlay_at_proximity(&address, 8);

        let topo = MockTopology::default().with_closest(vec![sideways]);
        assert!(closer_candidates(&topo, &address, requester, local).is_empty());
    }
}
