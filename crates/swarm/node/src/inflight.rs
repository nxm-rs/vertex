//! Non-blocking per-peer cap on concurrent outbound retrieval substreams.
//!
//! Retrieval fan-out clusters races onto a few close peers, so one hot peer can
//! accumulate more simultaneous outbound substreams than the remote's
//! per-connection multiplexer budget allows, and the remote resets them. This is
//! the non-economic overrun guard: it bounds concurrent retrieval substreams per
//! peer and is composed AFTER economic selection (selector, then skip-busy, then
//! throttle). It must never be merged with the affordability or debt signals.
//!
//! Skip-busy is non-blocking by construction: a peer at its cap is skipped at
//! selection time so the next-closest peer with a free slot serves the chunk,
//! never blocking on the head peer (which would reintroduce head-of-line
//! blocking). The permit rides the chosen request future and releases the slot on
//! drop, including a cancelled race leg.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use vertex_swarm_primitives::OverlayAddress;

/// Default per-peer cap on concurrent outbound retrieval substreams.
///
/// One conservative cap for every peer, sized to stay under the remote's
/// per-connection multiplexer budget (around 32 streams shared across retrieval,
/// pushsync, pricing, pseudosettle, swap, and identity). It is not keyed on peer
/// type: the multiplexer limit is type-independent and the type signals are
/// spoofable. A later handshake-negotiated higher vertex-to-vertex cap can
/// replace this constant.
pub const DEFAULT_PEER_INFLIGHT_CAP: NonZeroUsize = match NonZeroUsize::new(4) {
    Some(cap) => cap,
    None => unreachable!(),
};

/// Bounds concurrent outbound retrieval substreams per peer with non-blocking,
/// permit-on-drop reservations.
///
/// Cheap to clone-by-`Arc`; one instance is shared by the retrieval candidate
/// race (which reserves slots) and the client service (which forgets a peer on
/// disconnect).
pub struct PeerInflightLimiter {
    /// Uniform per-peer slot count.
    cap: NonZeroUsize,
    /// Per-peer semaphores keyed by overlay. A missing entry means the peer has
    /// its full cap free; the entry is created on first reservation and dropped
    /// on disconnect.
    peers: Mutex<HashMap<OverlayAddress, Arc<Semaphore>>>,
}

impl PeerInflightLimiter {
    /// Build a limiter capping every peer at `cap` concurrent outbound retrieval
    /// substreams.
    pub fn new(cap: NonZeroUsize) -> Self {
        Self {
            cap,
            peers: Mutex::new(HashMap::new()),
        }
    }

    /// Whether `peer` currently has a free retrieval slot.
    ///
    /// A peek used to skip busy peers at selection time. An unknown peer has its
    /// full cap free. The result can race a concurrent reservation, so the
    /// dispatching leg still reserves through [`Self::try_acquire`].
    pub fn has_free_slot(&self, peer: &OverlayAddress) -> bool {
        self.peers
            .lock()
            .get(peer)
            .is_none_or(|semaphore| semaphore.available_permits() > 0)
    }

    /// Reserve an outbound retrieval slot for `peer`, or `None` when it is at its
    /// cap.
    ///
    /// Non-blocking. The returned permit releases the slot on drop, including a
    /// cancelled race leg, so a slot is held only for the lifetime of the request
    /// future it rides.
    pub fn try_acquire(&self, peer: &OverlayAddress) -> Option<OwnedSemaphorePermit> {
        let semaphore = {
            let mut peers = self.peers.lock();
            Arc::clone(
                peers
                    .entry(*peer)
                    .or_insert_with(|| Arc::new(Semaphore::new(self.cap.get()))),
            )
        };
        semaphore.try_acquire_owned().ok()
    }

    /// Forget `peer`'s slot accounting on disconnect so memory does not grow with
    /// the count of distinct peers seen.
    ///
    /// Permits already handed out keep their own reference to the old semaphore
    /// and release harmlessly on drop; a later reconnect starts from a fresh cap.
    pub fn forget(&self, peer: &OverlayAddress) {
        self.peers.lock().remove(peer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: NonZeroUsize = match NonZeroUsize::new(2) {
        Some(cap) => cap,
        None => unreachable!(),
    };

    fn peer(n: u8) -> OverlayAddress {
        OverlayAddress::from([n; 32])
    }

    #[test]
    fn acquires_up_to_the_cap_then_skips() {
        let limiter = PeerInflightLimiter::new(CAP);
        let p = peer(1);

        let first = limiter.try_acquire(&p).expect("first slot free");
        let second = limiter.try_acquire(&p).expect("second slot free");
        assert!(!limiter.has_free_slot(&p), "at the cap, no free slot");
        assert!(
            limiter.try_acquire(&p).is_none(),
            "a peer at its cap is skipped, not blocked"
        );

        drop(first);
        drop(second);
    }

    #[test]
    fn dropping_a_permit_releases_the_slot() {
        let limiter = PeerInflightLimiter::new(CAP);
        let p = peer(2);

        let first = limiter.try_acquire(&p).expect("first slot");
        let second = limiter.try_acquire(&p).expect("second slot");
        assert!(limiter.try_acquire(&p).is_none(), "at the cap");

        // Releasing one permit frees exactly one slot, modelling a cancelled or
        // completed race leg returning a slot for the next request.
        drop(first);
        assert!(limiter.has_free_slot(&p), "a released slot is free again");
        let third = limiter.try_acquire(&p).expect("slot freed by the drop");

        drop(second);
        drop(third);
    }

    #[test]
    fn per_peer_slots_are_independent() {
        let limiter = PeerInflightLimiter::new(CAP);
        let a = peer(1);
        let b = peer(2);

        let _a1 = limiter.try_acquire(&a).expect("a slot");
        let _a2 = limiter.try_acquire(&a).expect("a slot");
        assert!(limiter.try_acquire(&a).is_none(), "a is at its cap");

        // b has its own untouched cap.
        assert!(limiter.has_free_slot(&b));
        let _b1 = limiter.try_acquire(&b).expect("b slot independent of a");
    }

    #[test]
    fn forget_resets_a_peer_to_a_fresh_cap() {
        let limiter = PeerInflightLimiter::new(CAP);
        let p = peer(3);

        let held = limiter.try_acquire(&p).expect("first slot");
        let _held2 = limiter.try_acquire(&p).expect("second slot");
        assert!(
            limiter.try_acquire(&p).is_none(),
            "at the cap before forget"
        );

        // A disconnect forgets the entry; a reconnect starts from a fresh cap even
        // while an old permit is still alive (it releases against the old
        // semaphore harmlessly).
        limiter.forget(&p);
        assert!(limiter.has_free_slot(&p), "forgotten peer has a fresh cap");
        let _fresh = limiter.try_acquire(&p).expect("fresh cap after forget");
        drop(held);
    }

    #[test]
    fn unknown_peer_has_a_free_slot() {
        let limiter = PeerInflightLimiter::new(DEFAULT_PEER_INFLIGHT_CAP);
        assert!(
            limiter.has_free_slot(&peer(9)),
            "an unseen peer is not busy"
        );
    }
}
