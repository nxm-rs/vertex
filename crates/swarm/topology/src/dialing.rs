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
use crate::behaviour::{BootnodeResolutionFuture, PendingBootnodeResolution, ResolvedBootnodes};
use crate::kademlia::RoutingCapacity;

use crate::behaviour::{DialTarget, TopologyBehaviour};

/// First interval between isolation probes; doubles per probe.
const ISOLATION_PROBE_INITIAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Ceiling on the isolation probe interval.
const ISOLATION_PROBE_MAX: std::time::Duration = std::time::Duration::from_secs(600);

/// Floor on the dnsaddr re-resolution interval, so a very short record TTL
/// cannot turn the refresh into a DNS hammer.
const DNSADDR_REFRESH_MIN: std::time::Duration = std::time::Duration::from_secs(30);

/// Ceiling on the dnsaddr re-resolution interval for very long record TTLs.
const DNSADDR_REFRESH_MAX: std::time::Duration = std::time::Duration::from_secs(3600);

/// Re-resolution interval when no lookup reported a TTL (resolution failed,
/// or the resolution path carries no TTL information).
const DNSADDR_REFRESH_FALLBACK: std::time::Duration = std::time::Duration::from_secs(300);

/// Interval until the next dnsaddr re-resolution: the earliest reported TTL
/// clamped to sane bounds, or the fallback when no TTL is known.
fn dnsaddr_refresh_interval(min_ttl: Option<std::time::Duration>) -> std::time::Duration {
    min_ttl.map_or(DNSADDR_REFRESH_FALLBACK, |ttl| {
        ttl.clamp(DNSADDR_REFRESH_MIN, DNSADDR_REFRESH_MAX)
    })
}

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
            self.gossip.mark_gossip_dial(peer_id);
        }

        self.pending_actions.push_back(ToSwarm::Dial { opts });
    }

    /// Re-dial the bootnodes when the table has drained to isolation:
    /// nothing connected or pending, no candidate queued, no resolution in
    /// flight. Without this the all-backoff empty table is a fixed point.
    /// The first probe fires immediately on detection; repeats back off
    /// exponentially. Any connection, pending handshake or queued candidate
    /// resets the probe. A dial still in flight may overlap one probe;
    /// `dial` deduplicates tracked peers so the overlap is a no-op.
    pub(crate) fn reconnect_if_isolated(&mut self) {
        if self.connection_registry.active_count() > 0
            || self.connection_registry.pending_count() > 0
            || self.routing.has_queued_candidates()
            || self.pending_bootnode_resolution.is_some()
        {
            self.isolation_probe = None;
            return;
        }

        let now = vertex_tasks::time::Instant::now();
        match &mut self.isolation_probe {
            Some((next_probe_at, delay)) => {
                if now < *next_probe_at {
                    return;
                }
                *delay = delay.saturating_mul(2).min(ISOLATION_PROBE_MAX);
                *next_probe_at = now + *delay;
            }
            None => {
                self.isolation_probe =
                    Some((now + ISOLATION_PROBE_INITIAL, ISOLATION_PROBE_INITIAL));
            }
        }

        info!("table drained to isolation; re-dialing bootnodes");
        metrics::counter!("topology_isolation_rebootstrap_total").increment(1);
        self.connect_bootnodes();
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
        match self.start_bootnode_resolution(bootnodes.clone(), trusted_peers.clone()) {
            Some(future) => {
                self.pending_bootnode_resolution = Some(PendingBootnodeResolution {
                    future,
                    refresh: false,
                })
            }
            None => self.dial_bootnodes(bootnodes, trusted_peers),
        }
    }

    /// Start the TTL-scheduled dnsaddr re-resolution.
    ///
    /// Unlike a connect, the completed refresh dials only addresses absent
    /// from the previous resolution, so a healthy node picks up a rotated
    /// bootnode address without redialing the stable set.
    pub(crate) fn refresh_bootnode_resolution(&mut self) {
        if self.pending_bootnode_resolution.is_some() {
            // The in-flight resolution re-arms the refresh timer on completion.
            return;
        }

        if let Some(future) =
            self.start_bootnode_resolution(self.bootnodes.clone(), self.trusted_peers.clone())
        {
            metrics::counter!("topology_dnsaddr_refresh_total").increment(1);
            self.pending_bootnode_resolution = Some(PendingBootnodeResolution {
                future,
                refresh: true,
            });
        }
    }

    /// Dial the outcome of a dnsaddr resolution and arm the next refresh.
    ///
    /// A connect dials every resolved address; a refresh dials only the
    /// addresses that were not in the previous resolution (`dial` additionally
    /// deduplicates peers that are already tracked).
    pub(crate) fn on_bootnode_resolution_complete(
        &mut self,
        resolved: ResolvedBootnodes,
        refresh: bool,
    ) {
        let ResolvedBootnodes {
            mut bootnodes,
            mut trusted,
            min_ttl,
        } = resolved;

        let next_resolved: std::collections::HashSet<Multiaddr> =
            bootnodes.iter().chain(trusted.iter()).cloned().collect();

        if refresh {
            bootnodes.retain(|addr| !self.resolved_bootnode_addrs.contains(addr));
            trusted.retain(|addr| !self.resolved_bootnode_addrs.contains(addr));
            if bootnodes.is_empty() && trusted.is_empty() {
                debug!("dnsaddr re-resolution found no new addresses");
            } else {
                info!(
                    new_bootnodes = bootnodes.len(),
                    new_trusted = trusted.len(),
                    "dnsaddr re-resolution found new addresses, dialing"
                );
            }
        } else {
            info!(
                bootnodes = bootnodes.len(),
                trusted = trusted.len(),
                "dnsaddr resolution complete, dialing bootnodes"
            );
        }

        self.resolved_bootnode_addrs = next_resolved;
        self.dial_bootnodes(bootnodes, trusted);

        let refresh_in = dnsaddr_refresh_interval(min_ttl);
        debug!(?refresh_in, "scheduling dnsaddr re-resolution");
        self.dnsaddr_refresh_timer = Some(Box::pin(vertex_tasks::time::sleep(refresh_in)));
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

    /// The shared dnsaddr resolver, built on first use and reused afterwards
    /// so DNS responses cache per record TTL across re-resolutions.
    #[cfg(not(target_arch = "wasm32"))]
    fn dnsaddr_resolver(&mut self) -> Option<vertex_net_dnsaddr::DnsaddrResolver> {
        if self.dnsaddr_resolver.is_none() {
            match vertex_net_dnsaddr::DnsaddrResolver::from_system_conf() {
                Ok(resolver) => self.dnsaddr_resolver = Some(resolver),
                Err(e) => warn!(error = %e, "Failed to build the dnsaddr resolver"),
            }
        }
        self.dnsaddr_resolver.clone()
    }

    /// Start resolving `/dnsaddr/` bootnode and trusted-peer entries to
    /// dialable multiaddrs, using the shared system resolver.
    ///
    /// Returns `None` when no entry needs resolution (or no resolver could be
    /// built) so the caller dials the literal addresses directly. Bootnodes
    /// and trusted peers are resolved separately so the caller can preserve
    /// the per-list dial reason.
    #[cfg(not(target_arch = "wasm32"))]
    fn start_bootnode_resolution(
        &mut self,
        bootnodes: Vec<Multiaddr>,
        trusted_peers: Vec<Multiaddr>,
    ) -> Option<BootnodeResolutionFuture> {
        use vertex_net_dnsaddr::is_dnsaddr;

        let needs_resolution =
            bootnodes.iter().any(is_dnsaddr) || trusted_peers.iter().any(is_dnsaddr);
        if !needs_resolution {
            return None;
        }

        let resolver = self.dnsaddr_resolver()?;

        info!(
            bootnodes = bootnodes.len(),
            trusted = trusted_peers.len(),
            "Resolving dnsaddr entries for bootnodes..."
        );

        Some(Box::pin(async move {
            let bootnodes = resolver.resolve_all(bootnodes.iter()).await;
            let trusted = resolver.resolve_all(trusted_peers.iter()).await;
            let min_ttl = match (bootnodes.min_ttl, trusted.min_ttl) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            ResolvedBootnodes {
                bootnodes: bootnodes.addrs,
                trusted: trusted.addrs,
                min_ttl,
            }
        }))
    }

    /// Start resolving `/dnsaddr/` bootnodes over DNS-over-HTTPS for the
    /// browser client.
    ///
    /// A browser cannot issue raw DNS TXT lookups, so the mainnet `/dnsaddr/`
    /// indirection is resolved over DoH (Cloudflare by default) with the
    /// embedded wss snapshot ([`vertex_swarm_spec::mainnet_wss_bootnodes`]) as
    /// the fallback whenever the live path yields nothing. DoH responses carry
    /// no TTL through this path, so re-resolution runs on the fallback
    /// interval. Trusted peers are expected to be literal browser-dialable
    /// multiaddrs; any `/dnsaddr/` trusted entry is dropped because the
    /// browser resolves only the mainnet name.
    ///
    /// Returns `None` when no bootnode needs resolution so the caller dials
    /// the literal addresses directly.
    #[cfg(target_arch = "wasm32")]
    fn start_bootnode_resolution(
        &mut self,
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
            ResolvedBootnodes {
                bootnodes: resolved,
                trusted: literal_trusted,
                min_ttl: None,
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashSet;
    use std::task::{Context, Poll};
    use std::time::Duration;

    use libp2p::swarm::ToSwarm;
    use vertex_swarm_api::{
        DefaultPeerConfig, SwarmNetworkConfig, SwarmPeerConfig, SwarmRoutingConfig,
    };
    use vertex_swarm_identity::Identity;
    use vertex_swarm_primitives::SwarmNodeType;

    use crate::TopologyBehaviourBuilder;
    use crate::kademlia::KademliaConfig;

    /// Minimal network configuration: no listeners, so the node is dial-only
    /// and its capability is pinned to dual-stack (bootnode dials pass the
    /// reachability filter).
    struct DialTestConfig {
        peers: DefaultPeerConfig,
        routing: KademliaConfig,
        empty_addrs: Vec<Multiaddr>,
    }

    impl DialTestConfig {
        fn new() -> Self {
            Self {
                peers: DefaultPeerConfig::default(),
                routing: KademliaConfig::default(),
                empty_addrs: Vec::new(),
            }
        }
    }

    impl SwarmNetworkConfig for DialTestConfig {
        fn listen_addrs(&self) -> &[Multiaddr] {
            &self.empty_addrs
        }
        fn bootnodes(&self) -> &[Multiaddr] {
            &self.empty_addrs
        }
        fn discovery_enabled(&self) -> bool {
            true
        }
        fn max_peers(&self) -> usize {
            32
        }
        fn idle_timeout(&self) -> Duration {
            Duration::from_secs(60)
        }
    }

    impl SwarmPeerConfig for DialTestConfig {
        type Peers = DefaultPeerConfig;
        fn peers(&self) -> &Self::Peers {
            &self.peers
        }
    }

    impl SwarmRoutingConfig for DialTestConfig {
        type Routing = KademliaConfig;
        fn routing(&self) -> &Self::Routing {
            &self.routing
        }
    }

    fn test_behaviour() -> TopologyBehaviour<Identity> {
        let identity = Identity::random(vertex_swarm_spec::init_testnet(), SwarmNodeType::Client);
        let (behaviour, _handle) = TopologyBehaviourBuilder::new(identity, &DialTestConfig::new())
            .try_build()
            .expect("build without runtime");
        behaviour
    }

    /// A public bootnode multiaddr carrying `/p2p/{peer_id}`.
    fn bootnode_addr(peer_id: PeerId, n: u8) -> Multiaddr {
        format!("/ip4/203.0.113.{n}/tcp/1634/p2p/{peer_id}")
            .parse()
            .expect("valid bootnode multiaddr")
    }

    /// Drain every queued dial action and return the peer ids dialed.
    fn drain_dials(behaviour: &mut TopologyBehaviour<Identity>) -> Vec<PeerId> {
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut dialed = Vec::new();
        loop {
            match libp2p::swarm::NetworkBehaviour::poll(behaviour, &mut cx) {
                Poll::Ready(ToSwarm::Dial { opts }) => {
                    if let Some(peer_id) = opts.get_peer_id() {
                        dialed.push(peer_id);
                    }
                }
                Poll::Ready(_) => {}
                Poll::Pending => break,
            }
        }
        dialed
    }

    /// A completed connect-resolution dials every resolved address, records
    /// the resolved set, and arms the TTL refresh timer.
    #[tokio::test]
    async fn connect_resolution_dials_all_and_arms_refresh() {
        let mut behaviour = test_behaviour();
        let (peer_a, peer_b) = (PeerId::random(), PeerId::random());
        let (addr_a, addr_b) = (bootnode_addr(peer_a, 1), bootnode_addr(peer_b, 2));

        behaviour.on_bootnode_resolution_complete(
            ResolvedBootnodes {
                bootnodes: vec![addr_a.clone(), addr_b.clone()],
                trusted: vec![],
                min_ttl: Some(Duration::from_secs(120)),
            },
            false,
        );

        let dialed = drain_dials(&mut behaviour);
        assert!(dialed.contains(&peer_a) && dialed.contains(&peer_b));
        assert_eq!(
            behaviour.resolved_bootnode_addrs,
            HashSet::from([addr_a, addr_b])
        );
        assert!(
            behaviour.dnsaddr_refresh_timer.is_some(),
            "a completed resolution must arm the re-resolution timer"
        );
    }

    /// A completed refresh dials only addresses absent from the previous
    /// resolution and replaces the recorded set, so a rotated-away address
    /// is not redialed on the next refresh.
    #[tokio::test]
    async fn refresh_dials_only_new_addresses() {
        let mut behaviour = test_behaviour();
        let (peer_old, peer_new) = (PeerId::random(), PeerId::random());
        let (addr_old, addr_new) = (bootnode_addr(peer_old, 1), bootnode_addr(peer_new, 2));

        // The previous resolution produced only the old address, and its dial
        // is no longer tracked (completed or failed since).
        behaviour.resolved_bootnode_addrs = HashSet::from([addr_old.clone()]);

        behaviour.on_bootnode_resolution_complete(
            ResolvedBootnodes {
                bootnodes: vec![addr_old.clone(), addr_new.clone()],
                trusted: vec![],
                min_ttl: None,
            },
            true,
        );

        let dialed = drain_dials(&mut behaviour);
        assert!(
            dialed.contains(&peer_new),
            "the rotated-in address is dialed"
        );
        assert!(
            !dialed.contains(&peer_old),
            "an address from the previous resolution is not redialed"
        );
        assert_eq!(
            behaviour.resolved_bootnode_addrs,
            HashSet::from([addr_old, addr_new])
        );
        assert!(behaviour.dnsaddr_refresh_timer.is_some());
    }

    /// A refresh whose result dropped an address forgets it, so the address
    /// counts as new again if a later rotation brings it back.
    #[tokio::test]
    async fn refresh_forgets_rotated_away_addresses() {
        let mut behaviour = test_behaviour();
        let (peer_old, peer_new) = (PeerId::random(), PeerId::random());
        let (addr_old, addr_new) = (bootnode_addr(peer_old, 1), bootnode_addr(peer_new, 2));
        behaviour.resolved_bootnode_addrs = HashSet::from([addr_old]);

        behaviour.on_bootnode_resolution_complete(
            ResolvedBootnodes {
                bootnodes: vec![addr_new.clone()],
                trusted: vec![],
                min_ttl: None,
            },
            true,
        );

        assert_eq!(behaviour.resolved_bootnode_addrs, HashSet::from([addr_new]));
    }

    /// The refresh interval honours the reported TTL inside sane bounds and
    /// falls back to a fixed interval when no TTL is known.
    #[test]
    fn refresh_interval_honours_ttl_within_bounds() {
        assert_eq!(
            dnsaddr_refresh_interval(Some(Duration::from_secs(120))),
            Duration::from_secs(120)
        );
        assert_eq!(
            dnsaddr_refresh_interval(Some(Duration::from_secs(1))),
            DNSADDR_REFRESH_MIN
        );
        assert_eq!(
            dnsaddr_refresh_interval(Some(Duration::from_secs(86_400))),
            DNSADDR_REFRESH_MAX
        );
        assert_eq!(dnsaddr_refresh_interval(None), DNSADDR_REFRESH_FALLBACK);
    }
}
