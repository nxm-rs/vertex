//! Dialing methods for topology behaviour.

use libp2p::Multiaddr;
use libp2p::PeerId;
use libp2p::swarm::ToSwarm;
use rand::seq::SliceRandom;
use tracing::{debug, info, trace, warn};
use vertex_net_dialer::PrepareError;
use vertex_net_dnsaddr::{is_dnsaddr, resolve_all};
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_primitives::SwarmNodeType;

use crate::DialReason;
use crate::gossip::GossipInput;
use crate::kademlia::RoutingCapacity;

use crate::behaviour::{DialTarget, TopologyBehaviour};

impl<I: SwarmIdentity + Clone> TopologyBehaviour<I> {
    /// Dial a known SwarmPeer for discovery.
    ///
    /// Checks routing capacity and filters before dialing.
    pub fn dial_swarm_peer(&mut self, swarm_peer: SwarmPeer) -> bool {
        let overlay = vertex_swarm_primitives::OverlayAddress::from(*swarm_peer.overlay());

        // Check if banned or in backoff
        if self.peer_manager.is_banned(&overlay) || self.peer_manager.peer_is_in_backoff(&overlay) {
            return false;
        }

        // Check scope compatibility
        if !self.can_advertise_to(&swarm_peer) {
            return false;
        }

        self.dial(DialTarget::Known(swarm_peer), DialReason::Discovery);
        true
    }

    /// Process a batch of dial requests.
    ///
    /// Returns the number of dials that were successfully initiated.
    pub fn dial_batch(&mut self, peers: impl IntoIterator<Item = SwarmPeer>) -> usize {
        let mut dialed = 0;
        for peer in peers {
            if self.dial_swarm_peer(peer) {
                dialed += 1;
            }
        }
        dialed
    }

    /// Dial a peer target.
    ///
    /// For Known peers: checks routing capacity, registers in DialTracker, verifies during handshake.
    /// For Unknown peers: no capacity check, tracked in pending_unknown_dials, learns overlay at handshake.
    pub(crate) fn dial(&mut self, target: DialTarget, reason: DialReason) {
        let Some(peer_id) = target.peer_id() else {
            warn!(?target, "Cannot dial: no /p2p/ component in address");
            return;
        };

        if self.is_peer_tracked(&peer_id) {
            trace!(%peer_id, "Skipping dial - already tracked");
            return;
        }

        // For Known peers, check routing capacity before dialing
        if let Some(overlay) = target.overlay() {
            if !self.routing.try_reserve_dial(&overlay, SwarmNodeType::Storer) {
                trace!(%overlay, "Skipping dial - at capacity or already tracking");
                return;
            }
        }

        // One call: filter addresses, build DialOpts, register in-flight
        let capability = self.nat_discovery.capability();
        let opts = match self.dial_tracker.prepare_and_start(
            target.overlay(),
            peer_id,
            target.addrs(),
            reason,
            |addr| vertex_net_local::is_dialable(addr, capability),
        ) {
            Ok(opts) => opts,
            Err(PrepareError::NoReachableAddresses) => {
                if let Some(overlay) = target.overlay() {
                    self.routing.release_dial(&overlay);
                    self.peer_manager.record_dial_failure(&overlay);
                }
                debug!(%peer_id, ?capability, "No reachable addresses");
                return;
            }
            Err(PrepareError::AlreadyTracked) => {
                if let Some(overlay) = target.overlay() {
                    self.routing.release_dial(&overlay);
                }
                trace!(%peer_id, "Skipping dial - already in dial tracker");
                return;
            }
            Err(PrepareError::InBackoff | PrepareError::Banned) => {
                if let Some(overlay) = target.overlay() {
                    self.routing.release_dial(&overlay);
                }
                trace!(%peer_id, "Skipping dial - peer in backoff or banned");
                return;
            }
        };

        debug!(%peer_id, ?reason, "Dialing peer");

        // Track discovery dials for delayed gossip exchange
        if reason == DialReason::Discovery {
            self.gossip.send(GossipInput::MarkGossipDial(peer_id));
        }

        self.pending_actions.push_back(ToSwarm::Dial { opts });
    }

    pub(crate) fn connect_bootnodes(&mut self) {
        let mut bootnodes = self.bootnodes.clone();
        bootnodes.shuffle(&mut rand::rng());
        let trusted_peers = self.trusted_peers.clone();

        if bootnodes.is_empty() && trusted_peers.is_empty() {
            return;
        }

        // Check if any addresses need dnsaddr resolution
        let needs_resolution = bootnodes.iter().any(|addr| is_dnsaddr(addr))
            || trusted_peers.iter().any(|addr| is_dnsaddr(addr));

        if needs_resolution {
            info!(
                bootnodes = bootnodes.len(),
                trusted = trusted_peers.len(),
                "Resolving dnsaddr entries for bootnodes..."
            );

            // Resolve bootnodes and trusted peers separately to preserve DialReason
            let future = Box::pin(async move {
                let resolved_bootnodes = resolve_all(bootnodes.iter()).await;
                let resolved_trusted = resolve_all(trusted_peers.iter()).await;
                (resolved_bootnodes, resolved_trusted)
            });
            self.pending_bootnode_resolution = Some(future);
        } else {
            // No resolution needed, dial immediately
            self.dial_bootnodes(bootnodes, trusted_peers);
        }
    }

    /// Dial bootnodes and trusted peers (called after dnsaddr resolution if needed).
    pub(crate) fn dial_bootnodes(&mut self, bootnodes: Vec<Multiaddr>, trusted_peers: Vec<Multiaddr>) {
        if !bootnodes.is_empty() {
            info!(count = bootnodes.len(), "Connecting to all bootnodes...");
        }

        for addr in bootnodes {
            self.dial(DialTarget::Unknown(addr), DialReason::Bootnode);
        }

        for addr in trusted_peers {
            self.dial(DialTarget::Unknown(addr), DialReason::Trusted);
        }
    }

    /// Check if a PeerId is already being tracked (dialing, connected, or active).
    pub(crate) fn is_peer_tracked(&self, peer_id: &PeerId) -> bool {
        self.connection_registry.contains_peer(peer_id)
            || self.dial_tracker.contains_peer(peer_id)
    }
}
