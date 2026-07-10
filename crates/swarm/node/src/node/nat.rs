//! NAT traversal and LAN discovery for native node types.
//!
//! [`NatBehaviour`] composes AutoNAT v2 (client + server), UPnP, and mDNS into
//! a single sub-behaviour so the node composites carry one platform-neutral
//! field. The browser client dials over websockets and never listens, so it
//! has no NAT or LAN-discovery surface; the wasm sibling module (`nat_wasm.rs`)
//! exposes the same item names and signatures over a no-op behaviour.

use std::collections::HashMap;
use std::io;
use std::task::{Context, Poll};
use std::time::Duration;

use libp2p::autonat::v2 as autonat;
use libp2p::core::Endpoint;
use libp2p::core::transport::PortUse;
use libp2p::mdns;
use libp2p::multiaddr::Protocol;
use libp2p::swarm::behaviour::ConnectionEstablished;
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::swarm::dial_opts::DialOpts;
use libp2p::swarm::{
    ConnectionClosed, ConnectionDenied, ConnectionId, DialError, DialFailure, FromSwarm,
    NetworkBehaviour, THandler, THandlerInEvent, THandlerOutEvent, ToSwarm,
};
use libp2p::upnp;
use libp2p::{Multiaddr, PeerId};
use tracing::{debug, info, warn};
use vertex_net_ratelimiter::{Quota, RateLimiter};
use vertex_swarm_api::{AutonatServerQuota, SwarmIdentity, SwarmNetworkConfig};
use vertex_swarm_topology::{TopologyBehaviour, TopologyCommand};

/// NAT traversal (AutoNAT v2, UPnP) and LAN discovery (mDNS), composed as one
/// sub-behaviour so the node composites carry a single platform-neutral field.
///
/// AutoNAT v2 (client + server) and UPnP run in the same swarm as identify, so
/// the libp2p swarm propagates verified external addresses between them
/// automatically. Each is wrapped in a [`Toggle`] so an operator can disable
/// it without changing the behaviour type.
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "NatEvent")]
pub(crate) struct NatBehaviour {
    autonat_client: Toggle<autonat::client::Behaviour>,
    autonat_server: Toggle<BoundedAutonatServer>,
    upnp: Toggle<upnp::tokio::Behaviour>,
    mdns: Toggle<mdns::tokio::Behaviour>,
}

impl NatBehaviour {
    /// Build the NAT behaviours from a network configuration.
    ///
    /// AutoNAT v2 and mDNS are enabled by default for every node type; UPnP is
    /// opt-in. mDNS needs the local [`PeerId`], so the behaviour is built where
    /// the swarm's public key is available.
    pub(crate) fn from_config(config: &impl SwarmNetworkConfig, local_peer_id: PeerId) -> Self {
        let autonat = config.autonat_enabled();
        Self {
            autonat_client: Toggle::from(autonat.then(autonat::client::Behaviour::default)),
            autonat_server: Toggle::from(
                autonat.then(|| BoundedAutonatServer::new(config.autonat_server_quota())),
            ),
            upnp: Toggle::from(config.upnp_enabled().then(upnp::tokio::Behaviour::default)),
            mdns: build_mdns_toggle(config.mdns_enabled(), local_peer_id),
        }
    }
}

/// AutoNAT v2 server bounded by the configured dial-back budget.
///
/// Any connected peer can ask the server to dial its addresses back, so the
/// requested dial is admitted against a GCRA rate bucket plus an in-flight cap
/// before it reaches the swarm. A refused dial-back resolves the pending
/// request as a dial error, which the client reads as an inconclusive probe.
///
/// The inner server reports the request conversation, not the dial outcome:
/// its event carries `Ok` even when the dial-back was refused or its dial
/// failed. Such events are downgraded to an error here so the reachability
/// promotion in [`handle_autonat_server_event`] only ever fires for a
/// performed dial-back. The downgrade is matched per client and biases
/// towards under-promotion, never over-promotion.
pub(crate) struct BoundedAutonatServer {
    inner: autonat::server::Behaviour,
    limiter: RateLimiter,
    max_in_flight: usize,
    /// Dial-back dials forwarded to the swarm and not yet resolved.
    in_flight: HashMap<ConnectionId, Option<PeerId>>,
    /// Per-client count of refused or dial-failed dial-backs whose server
    /// events are still pending downgrade.
    not_performed: HashMap<PeerId, u32>,
}

impl BoundedAutonatServer {
    fn new(quota: AutonatServerQuota) -> Self {
        Self {
            inner: autonat::server::Behaviour::default(),
            limiter: RateLimiter::new(Quota::n_every(
                quota.dial_backs_per_minute,
                Duration::from_secs(60),
            )),
            max_in_flight: quota.max_in_flight.get() as usize,
            in_flight: HashMap::new(),
            not_performed: HashMap::new(),
        }
    }

    /// Admit one dial-back against the budget, reserving an in-flight slot on
    /// success. The slot is released by the dial's `ConnectionEstablished` or
    /// `DialFailure`.
    fn admit(&mut self, connection_id: ConnectionId, peer: Option<PeerId>) -> bool {
        if self.in_flight.len() >= self.max_in_flight {
            return false;
        }
        if self.limiter.try_consume().is_err() {
            return false;
        }
        self.in_flight.insert(connection_id, peer);
        true
    }

    /// Refuse a dial-back the inner behaviour requested. The injected dial
    /// failure resolves the pending request, and the request handler replies
    /// to the client with a dial error; no dial ever reaches the swarm.
    fn refuse(&mut self, opts: DialOpts) {
        let peer = opts.get_peer_id();
        self.inner
            .on_swarm_event(FromSwarm::DialFailure(DialFailure {
                peer_id: peer,
                error: &DialError::Aborted,
                connection_id: opts.connection_id(),
            }));
        self.mark_not_performed(peer);
        debug!(client = ?peer, "Refusing AutoNAT dial-back: budget exhausted");
    }

    fn mark_not_performed(&mut self, peer: Option<PeerId>) {
        if let Some(peer) = peer {
            *self.not_performed.entry(peer).or_insert(0) += 1;
        }
    }

    /// Consume one pending downgrade marker for `client`, if any.
    fn take_not_performed(&mut self, client: &PeerId) -> bool {
        let Some(count) = self.not_performed.get_mut(client) else {
            return false;
        };
        *count -= 1;
        if *count == 0 {
            self.not_performed.remove(client);
        }
        true
    }
}

impl NetworkBehaviour for BoundedAutonatServer {
    type ConnectionHandler = <autonat::server::Behaviour as NetworkBehaviour>::ConnectionHandler;
    type ToSwarm = autonat::server::Event;

    fn handle_pending_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<(), ConnectionDenied> {
        self.inner
            .handle_pending_inbound_connection(connection_id, local_addr, remote_addr)
    }

    fn handle_pending_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        maybe_peer: Option<PeerId>,
        addresses: &[Multiaddr],
        effective_role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        self.inner.handle_pending_outbound_connection(
            connection_id,
            maybe_peer,
            addresses,
            effective_role,
        )
    }

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.inner.handle_established_inbound_connection(
            connection_id,
            peer,
            local_addr,
            remote_addr,
        )
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role_override: Endpoint,
        port_use: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.inner.handle_established_outbound_connection(
            connection_id,
            peer,
            addr,
            role_override,
            port_use,
        )
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        match &event {
            FromSwarm::ConnectionEstablished(ConnectionEstablished { connection_id, .. }) => {
                self.in_flight.remove(connection_id);
            }
            FromSwarm::DialFailure(DialFailure {
                peer_id,
                connection_id,
                ..
            }) => {
                if let Some(peer) = self.in_flight.remove(connection_id) {
                    self.mark_not_performed(peer.or(*peer_id));
                }
            }
            FromSwarm::ConnectionClosed(ConnectionClosed {
                peer_id,
                remaining_established: 0,
                ..
            }) => {
                // No handler is left to surface events for this client, so
                // any pending downgrade markers are moot.
                self.not_performed.remove(peer_id);
            }
            _ => {}
        }
        self.inner.on_swarm_event(event);
    }

    fn on_connection_handler_event(
        &mut self,
        peer: PeerId,
        connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        self.inner
            .on_connection_handler_event(peer, connection_id, event);
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        loop {
            match self.inner.poll(cx) {
                Poll::Ready(ToSwarm::Dial { opts }) => {
                    if self.admit(opts.connection_id(), opts.get_peer_id()) {
                        return Poll::Ready(ToSwarm::Dial { opts });
                    }
                    self.refuse(opts);
                }
                Poll::Ready(ToSwarm::GenerateEvent(mut event)) => {
                    if event.result.is_ok() && self.take_not_performed(&event.client) {
                        event.result = Err(io::Error::other("dial-back was not performed"));
                    }
                    return Poll::Ready(ToSwarm::GenerateEvent(event));
                }
                Poll::Ready(other) => return Poll::Ready(other),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Events emitted by [`NatBehaviour`].
pub(crate) enum NatEvent {
    AutonatClient(autonat::client::Event),
    AutonatServer(autonat::server::Event),
    Upnp(upnp::Event),
    Mdns(mdns::Event),
}

impl From<autonat::client::Event> for NatEvent {
    fn from(event: autonat::client::Event) -> Self {
        NatEvent::AutonatClient(event)
    }
}

impl From<autonat::server::Event> for NatEvent {
    fn from(event: autonat::server::Event) -> Self {
        NatEvent::AutonatServer(event)
    }
}

impl From<upnp::Event> for NatEvent {
    fn from(event: upnp::Event) -> Self {
        NatEvent::Upnp(event)
    }
}

impl From<mdns::Event> for NatEvent {
    fn from(event: mdns::Event) -> Self {
        NatEvent::Mdns(event)
    }
}

/// Dispatch a [`NatEvent`] to the matching handler.
pub(crate) fn handle_nat_event<I: SwarmIdentity + Clone>(
    local_peer_id: PeerId,
    topology: &mut TopologyBehaviour<I>,
    event: NatEvent,
) {
    match event {
        NatEvent::AutonatClient(event) => handle_autonat_client_event(event),
        NatEvent::AutonatServer(event) => handle_autonat_server_event(topology, event),
        NatEvent::Upnp(event) => handle_upnp_event(event),
        NatEvent::Mdns(event) => handle_mdns_event(local_peer_id, topology, event),
    }
}

/// Build the mDNS discovery behaviour as a [`Toggle`].
///
/// mDNS construction is fallible (it binds a multicast socket) and needs the
/// local [`PeerId`]. A bind failure never aborts node startup: the behaviour
/// is logged and left disabled.
fn build_mdns_toggle(enabled: bool, peer_id: PeerId) -> Toggle<mdns::tokio::Behaviour> {
    if !enabled {
        return Toggle::from(None);
    }
    match mdns::tokio::Behaviour::new(mdns::Config::default(), peer_id) {
        Ok(behaviour) => Toggle::from(Some(behaviour)),
        Err(error) => {
            warn!(%error, "mDNS discovery disabled: failed to start multicast listener");
            Toggle::from(None)
        }
    }
}

/// Turn an mDNS-discovered `(peer, addr)` pair into a dialable multiaddr.
///
/// Returns `None` for our own [`PeerId`] (mDNS hears its own announcements).
/// Otherwise appends `/p2p/<peer_id>` when the address lacks a peer component
/// so the dial resolves to a concrete peer; an address that already carries a
/// `/p2p/` is returned unchanged.
fn mdns_dial_addr(local_peer_id: &PeerId, peer_id: PeerId, addr: Multiaddr) -> Option<Multiaddr> {
    if peer_id == *local_peer_id {
        return None;
    }
    let has_p2p = addr.iter().any(|p| matches!(p, Protocol::P2p(_)));
    if has_p2p {
        Some(addr)
    } else {
        Some(addr.with(Protocol::P2p(peer_id)))
    }
}

/// Handle an mDNS event by dialing freshly discovered LAN peers.
///
/// `Discovered` peers are dialed as `DialTarget::Unknown` through the topology;
/// the overlay address is learned at the Swarm handshake. `Expired` is only
/// logged: an mDNS TTL lapse is not connection state and must not tear down a
/// live connection.
fn handle_mdns_event<I: SwarmIdentity + Clone>(
    local_peer_id: PeerId,
    topology: &mut TopologyBehaviour<I>,
    event: mdns::Event,
) {
    match event {
        mdns::Event::Discovered(peers) => {
            for (peer_id, addr) in peers {
                if let Some(dial_addr) = mdns_dial_addr(&local_peer_id, peer_id, addr) {
                    debug!(%peer_id, %dial_addr, "Dialing mDNS-discovered LAN peer");
                    topology.on_command(TopologyCommand::Dial(dial_addr));
                }
            }
        }
        mdns::Event::Expired(peers) => {
            for (peer_id, addr) in peers {
                debug!(%peer_id, %addr, "mDNS record expired");
            }
        }
    }
}

/// Handle an AutoNAT v2 server event by promoting verified peers.
///
/// A successful dial-back proves the `client` peer accepts inbound
/// connections, so we forward it into the topology reachability tracker.
fn handle_autonat_server_event<I: SwarmIdentity + Clone>(
    topology: &TopologyBehaviour<I>,
    event: autonat::server::Event,
) {
    match event.result {
        Ok(()) => {
            debug!(client = %event.client, tested_addr = %event.tested_addr, "AutoNAT dial-back succeeded");
            topology.on_autonat_peer_confirmed(event.client);
        }
        Err(error) => {
            debug!(client = %event.client, tested_addr = %event.tested_addr, %error, "AutoNAT dial-back failed");
        }
    }
}

/// Handle an AutoNAT v2 client event (verification of our own addresses).
///
/// On success the swarm marks the address confirmed and broadcasts
/// `FromSwarm::ExternalAddrConfirmed`, which the topology behaviour consumes to
/// flip public connectivity. Here we only log the outcome.
fn handle_autonat_client_event(event: autonat::client::Event) {
    match event.result {
        Ok(()) => debug!(
            server = %event.server,
            tested_addr = %event.tested_addr,
            "AutoNAT confirmed our address is publicly reachable"
        ),
        Err(error) => debug!(
            server = %event.server,
            tested_addr = %event.tested_addr,
            %error,
            "AutoNAT could not confirm our address"
        ),
    }
}

/// Handle a UPnP event. Port-map confirmations reach the topology behaviour as
/// `FromSwarm::ExternalAddrConfirmed`; here we only surface operator-facing
/// gateway diagnostics.
fn handle_upnp_event(event: upnp::Event) {
    match event {
        upnp::Event::NewExternalAddr { external_addr, .. } => {
            info!(%external_addr, "UPnP mapped external address")
        }
        upnp::Event::ExpiredExternalAddr { external_addr, .. } => {
            debug!(%external_addr, "UPnP external address expired")
        }
        upnp::Event::GatewayNotFound => debug!("UPnP gateway not found"),
        upnp::Event::NonRoutableGateway => debug!("UPnP gateway is not publicly routable"),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::num::NonZeroU32;

    use futures::StreamExt;
    use libp2p::core::ConnectedPoint;
    use libp2p::identity::Keypair;
    use libp2p::swarm::{Swarm, SwarmEvent};
    use libp2p_swarm_test::SwarmExt;
    use rand_08::rngs::OsRng;
    use vertex_swarm_net_identify as identify;

    use super::*;

    fn random_peer_id() -> PeerId {
        Keypair::generate_ed25519().public().to_peer_id()
    }

    fn quota(per_minute: u32, in_flight: u32) -> AutonatServerQuota {
        AutonatServerQuota {
            dial_backs_per_minute: NonZeroU32::new(per_minute).expect("non-zero"),
            max_in_flight: NonZeroU32::new(in_flight).expect("non-zero"),
        }
    }

    fn conn(n: usize) -> ConnectionId {
        ConnectionId::new_unchecked(n)
    }

    fn dial_established(server: &mut BoundedAutonatServer, peer: PeerId, id: ConnectionId) {
        let endpoint = ConnectedPoint::Dialer {
            address: "/ip4/203.0.113.7/tcp/1634".parse().expect("valid addr"),
            role_override: Endpoint::Dialer,
            port_use: PortUse::New,
        };
        server.on_swarm_event(FromSwarm::ConnectionEstablished(ConnectionEstablished {
            peer_id: peer,
            connection_id: id,
            endpoint: &endpoint,
            failed_addresses: &[],
            other_established: 0,
        }));
    }

    #[test]
    fn in_flight_cap_admits_and_releases() {
        let mut server = BoundedAutonatServer::new(quota(1000, 2));
        let peer = random_peer_id();

        assert!(server.admit(conn(1), Some(peer)));
        assert!(server.admit(conn(2), Some(peer)));
        assert!(!server.admit(conn(3), Some(peer)), "cap reached");

        dial_established(&mut server, peer, conn(1));
        assert!(server.admit(conn(3), Some(peer)), "slot released");
    }

    #[test]
    fn rate_cap_refuses_beyond_burst() {
        let mut server = BoundedAutonatServer::new(quota(2, 100));
        let peer = random_peer_id();

        assert!(server.admit(conn(1), Some(peer)));
        assert!(server.admit(conn(2), Some(peer)));
        dial_established(&mut server, peer, conn(1));
        dial_established(&mut server, peer, conn(2));

        // Slots are free but the minute bucket is drained.
        assert!(!server.admit(conn(3), Some(peer)));
    }

    #[test]
    fn dial_failure_releases_the_slot_and_marks_the_client() {
        let mut server = BoundedAutonatServer::new(quota(1000, 1));
        let peer = random_peer_id();

        assert!(server.admit(conn(1), Some(peer)));
        assert!(!server.admit(conn(2), Some(peer)));

        server.on_swarm_event(FromSwarm::DialFailure(DialFailure {
            peer_id: Some(peer),
            error: &DialError::Aborted,
            connection_id: conn(1),
        }));

        assert!(server.admit(conn(2), Some(peer)), "slot released");
        assert!(server.take_not_performed(&peer), "failed dial marked");
        assert!(!server.take_not_performed(&peer), "marker consumed");
    }

    #[test]
    fn disconnect_prunes_pending_downgrade_markers() {
        let mut server = BoundedAutonatServer::new(quota(1000, 1));
        let peer = random_peer_id();
        server.mark_not_performed(Some(peer));

        let endpoint = ConnectedPoint::Dialer {
            address: "/ip4/203.0.113.7/tcp/1634".parse().expect("valid addr"),
            role_override: Endpoint::Dialer,
            port_use: PortUse::New,
        };
        server.on_swarm_event(FromSwarm::ConnectionClosed(ConnectionClosed {
            peer_id: peer,
            connection_id: conn(1),
            endpoint: &endpoint,
            cause: None,
            remaining_established: 0,
        }));

        assert!(!server.take_not_performed(&peer), "markers pruned");
    }

    /// Minimal node mirroring the production composition: vertex identify (the
    /// candidate source), the AutoNAT v2 client, and the bounded server.
    #[derive(NetworkBehaviour)]
    struct TestNode {
        identify: identify::Behaviour,
        autonat_client: autonat::client::Behaviour,
        autonat_server: BoundedAutonatServer,
    }

    fn new_node(server_quota: AutonatServerQuota) -> Swarm<TestNode> {
        Swarm::new_ephemeral_tokio(|keypair| TestNode {
            identify: identify::Behaviour::new(
                identify::Config::new(keypair.public().clone()),
                identify::new_agent_versions(),
                identify::ObservedAddresses::default(),
            ),
            autonat_client: autonat::client::Behaviour::new(
                OsRng,
                autonat::client::Config::default().with_probe_interval(Duration::from_millis(100)),
            ),
            autonat_server: BoundedAutonatServer::new(server_quota),
        })
    }

    /// The bound is transparent while the budget holds: a dial-back completes
    /// and the client's address is confirmed, exactly as with the raw server.
    #[tokio::test]
    async fn dial_back_completes_within_budget() {
        let mut server = new_node(AutonatServerQuota::default());
        let mut client = new_node(AutonatServerQuota::default());

        server.listen().with_tcp_addr_external().await;
        client.listen().await;
        let client_peer = *client.local_peer_id();
        client.connect(&mut server).await;

        let server_task = async {
            loop {
                if let SwarmEvent::Behaviour(TestNodeEvent::AutonatServer(event)) =
                    server.select_next_some().await
                    && event.client == client_peer
                    && event.result.is_ok()
                {
                    break;
                }
            }
        };
        let client_task = async {
            loop {
                if let SwarmEvent::ExternalAddrConfirmed { .. } = client.select_next_some().await {
                    break;
                }
            }
        };

        tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(server_task, client_task)
        })
        .await
        .expect("dial-back within budget did not complete within 30s");
    }

    /// An exhausted budget refuses the dial-back: the client's probe fails and
    /// the server event is downgraded so the refusal can never surface as a
    /// successful reachability test.
    #[tokio::test]
    async fn exhausted_budget_refuses_and_downgrades_the_event() {
        let mut server = new_node(quota(1, 1));
        // Drain the single per-minute token so every request in this test is
        // over budget.
        server
            .behaviour_mut()
            .autonat_server
            .limiter
            .try_consume()
            .expect("bucket starts full");
        let mut client = new_node(AutonatServerQuota::default());

        server.listen().with_tcp_addr_external().await;
        client.listen().await;
        let client_peer = *client.local_peer_id();
        client.connect(&mut server).await;

        let server_task = async {
            loop {
                if let SwarmEvent::Behaviour(TestNodeEvent::AutonatServer(event)) =
                    server.select_next_some().await
                    && event.client == client_peer
                {
                    break event;
                }
            }
        };
        let client_task = async {
            loop {
                if let SwarmEvent::Behaviour(TestNodeEvent::AutonatClient(event)) =
                    client.select_next_some().await
                {
                    break event;
                }
            }
        };

        let (server_event, client_event) = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(server_task, client_task)
        })
        .await
        .expect("refusal did not surface within 30s");

        assert!(
            server_event.result.is_err(),
            "refused dial-back must not look like a successful test"
        );
        assert!(
            client_event.result.is_err(),
            "client reads the refusal as a failed probe"
        );
    }

    #[test]
    fn mdns_dial_addr_appends_p2p_when_missing() {
        let local = random_peer_id();
        let peer = random_peer_id();
        let addr: Multiaddr = "/ip4/192.168.1.10/tcp/1634".parse().expect("valid addr");

        let dial = mdns_dial_addr(&local, peer, addr).expect("peer should be dialable");

        let expected: Multiaddr = format!("/ip4/192.168.1.10/tcp/1634/p2p/{peer}")
            .parse()
            .expect("valid addr");
        assert_eq!(dial, expected);
    }

    #[test]
    fn mdns_dial_addr_keeps_existing_p2p() {
        let local = random_peer_id();
        let peer = random_peer_id();
        let addr: Multiaddr = format!("/ip4/192.168.1.10/tcp/1634/p2p/{peer}")
            .parse()
            .expect("valid addr");

        let dial = mdns_dial_addr(&local, peer, addr.clone()).expect("peer should be dialable");
        assert_eq!(dial, addr);
    }

    #[test]
    fn mdns_dial_addr_skips_self() {
        let local = random_peer_id();
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/1634".parse().expect("valid addr");

        assert!(mdns_dial_addr(&local, local, addr).is_none());
    }
}
