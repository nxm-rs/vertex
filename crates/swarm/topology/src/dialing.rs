//! Dialing methods for topology behaviour.

use libp2p::Multiaddr;
use libp2p::PeerId;
use libp2p::swarm::ToSwarm;
use rand::seq::SliceRandom;
use tracing::{debug, info, trace, warn};
use vertex_net_dialer::error::PrepareError;
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_primitives::SwarmNodeType;
use vertex_util_runtime::rand::non_crypto_rng;

use crate::DialReason;
use crate::behaviour::BootnodeResolutionFuture;
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
        if let Some(overlay) = target.overlay()
            && !self
                .routing
                .try_reserve_dial(&overlay, SwarmNodeType::Storer)
        {
            trace!(%overlay, "Skipping dial - at capacity or already tracking");
            return;
        }

        // One call: filter addresses, build DialOpts, register in-flight.
        // The filter covers both halves of dialability: IP-family
        // reachability and whether the assembled transport stack supports
        // the address shape at all (TCP natively, secure websockets in the
        // browser).
        let capability = self.nat_discovery.dial_capability();
        let opts = match self.dial_tracker.prepare_and_start(
            target.overlay(),
            peer_id,
            target.addrs(),
            reason,
            |addr| capability.can_dial(addr),
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
        bootnodes.shuffle(&mut non_crypto_rng());
        let trusted_peers = self.trusted_peers.clone();

        if bootnodes.is_empty() && trusted_peers.is_empty() {
            return;
        }

        // `/dnsaddr/` entries need resolution to dialable multiaddrs before they
        // can be dialed. Native does this over the system resolver; the browser
        // does it over DNS-over-HTTPS. When nothing needs resolving the helper
        // returns `None` and we dial the literal addresses immediately.
        match start_bootnode_resolution(bootnodes.clone(), trusted_peers.clone()) {
            Some(future) => self.pending_bootnode_resolution = Some(future),
            None => self.dial_bootnodes(bootnodes, trusted_peers),
        }
    }

    /// Dial bootnodes and trusted peers (called after dnsaddr resolution if needed).
    pub(crate) fn dial_bootnodes(
        &mut self,
        bootnodes: Vec<Multiaddr>,
        trusted_peers: Vec<Multiaddr>,
    ) {
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
        self.connection_registry.contains_peer(peer_id) || self.dial_tracker.contains_peer(peer_id)
    }
}

/// Start resolving `/dnsaddr/` bootnode and trusted-peer entries to dialable
/// multiaddrs, using the system resolver.
///
/// Returns `None` when no entry needs resolution so the caller dials the literal
/// addresses directly. Bootnodes and trusted peers are resolved separately so
/// the caller can preserve the per-list dial reason.
#[cfg(not(target_arch = "wasm32"))]
fn start_bootnode_resolution(
    bootnodes: Vec<Multiaddr>,
    trusted_peers: Vec<Multiaddr>,
) -> Option<BootnodeResolutionFuture> {
    use vertex_net_dnsaddr::{is_dnsaddr, resolve_all};

    let needs_resolution = bootnodes.iter().any(is_dnsaddr) || trusted_peers.iter().any(is_dnsaddr);
    if !needs_resolution {
        return None;
    }

    info!(
        bootnodes = bootnodes.len(),
        trusted = trusted_peers.len(),
        "Resolving dnsaddr entries for bootnodes..."
    );

    Some(Box::pin(async move {
        let resolved_bootnodes = resolve_all(bootnodes.iter()).await;
        let resolved_trusted = resolve_all(trusted_peers.iter()).await;
        (resolved_bootnodes, resolved_trusted)
    }))
}

/// Start resolving `/dnsaddr/` bootnodes over DNS-over-HTTPS for the browser
/// client.
///
/// A browser cannot issue raw DNS TXT lookups, so the mainnet `/dnsaddr/`
/// indirection is resolved over DoH (Cloudflare by default) with the embedded
/// wss snapshot ([`vertex_swarm_spec::mainnet_wss_bootnodes`]) as the fallback
/// whenever the live path yields nothing. Trusted peers are expected to be
/// literal browser-dialable multiaddrs; any `/dnsaddr/` trusted entry is dropped
/// because the browser resolves only the mainnet name.
///
/// Returns `None` when no bootnode needs resolution so the caller dials the
/// literal addresses directly.
#[cfg(target_arch = "wasm32")]
fn start_bootnode_resolution(
    bootnodes: Vec<Multiaddr>,
    trusted_peers: Vec<Multiaddr>,
) -> Option<BootnodeResolutionFuture> {
    use libp2p::multiaddr::Protocol;
    use vertex_net_dnsaddr_doh::{DohClient, resolve_mainnet_wss_bootnodes};

    let is_dnsaddr = |addr: &Multiaddr| addr.iter().any(|p| matches!(p, Protocol::Dnsaddr(_)));

    let needs_resolution = bootnodes.iter().any(is_dnsaddr);
    let literal_trusted: Vec<Multiaddr> = trusted_peers
        .into_iter()
        .filter(|a| !is_dnsaddr(a))
        .collect();
    let literal_bootnodes: Vec<Multiaddr> = bootnodes
        .iter()
        .filter(|a| !is_dnsaddr(a))
        .cloned()
        .collect();

    if !needs_resolution {
        return None;
    }

    info!(
        bootnodes = bootnodes.len(),
        "Resolving dnsaddr bootnodes over DNS-over-HTTPS..."
    );

    Some(Box::pin(async move {
        let client = DohClient::default();
        let mut resolved =
            resolve_mainnet_wss_bootnodes(&client, vertex_swarm_spec::mainnet_wss_bootnodes())
                .await;
        resolved.extend(literal_bootnodes);
        (resolved, literal_trusted)
    }))
}
