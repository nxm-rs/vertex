//! NAT traversal and LAN discovery for native node types.
//!
//! [`NatBehaviour`] composes AutoNAT v2 (client + server), UPnP, a circuit
//! relay v2 server, DCUtR, and mDNS into a single sub-behaviour so the node
//! composites carry one platform-neutral field. The circuit relay v2 client
//! ([`RelayClientBehaviour`]) is a sibling composite field rather than a
//! `NatBehaviour` member because the swarm transport assembly constructs it
//! together with the relay transport. The browser client dials over
//! websockets and never listens, so it has no NAT or LAN-discovery surface;
//! the wasm sibling module (`nat_wasm.rs`) exposes the same item names and
//! signatures over no-op behaviours.

use libp2p::autonat::v2 as autonat;
use libp2p::dcutr;
use libp2p::mdns;
use libp2p::multiaddr::Protocol;
use libp2p::relay;
use libp2p::swarm::NetworkBehaviour;
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::upnp;
use libp2p::{Multiaddr, PeerId};
use tracing::{debug, info, warn};
use vertex_swarm_api::{SwarmIdentity, SwarmNetworkConfig};
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_topology::{TopologyBehaviour, TopologyCommand};

/// Circuit relay v2 client behaviour, paired with the relay transport the
/// swarm assembly injects. The pair shares state over a channel, so the
/// handle must be composed into the node behaviour or relayed dials stall.
pub(crate) type RelayClientBehaviour = relay::client::Behaviour;

/// Events from the relay-client behaviour.
pub(crate) type RelayClientEvent = relay::client::Event;

/// NAT traversal (AutoNAT v2, UPnP, DCUtR), the circuit relay v2 server, and
/// LAN discovery (mDNS), composed as one sub-behaviour so the node composites
/// carry a single platform-neutral field.
///
/// AutoNAT v2 (client + server) and UPnP run in the same swarm as identify, so
/// the libp2p swarm propagates verified external addresses between them
/// automatically. Each is wrapped in a [`Toggle`] so an operator can disable
/// it without changing the behaviour type.
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "NatEvent")]
pub(crate) struct NatBehaviour {
    autonat_client: Toggle<autonat::client::Behaviour>,
    autonat_server: Toggle<autonat::server::Behaviour>,
    relay_server: Toggle<relay::Behaviour>,
    dcutr: Toggle<dcutr::Behaviour>,
    upnp: Toggle<upnp::tokio::Behaviour>,
    mdns: Toggle<mdns::tokio::Behaviour>,
}

impl NatBehaviour {
    /// Build the NAT behaviours from a network configuration.
    ///
    /// AutoNAT v2 and mDNS are enabled by default for every node type; UPnP is
    /// opt-in. The circuit relay v2 server is off by default and enabled for
    /// the bootnode and storer node types, which listen on publicly reachable
    /// multiaddrs; reservations and circuits are bounded by the libp2p relay
    /// defaults (reservation and circuit caps, per-peer and per-IP rate
    /// limits, circuit duration and byte limits). DCUtR runs on the client and
    /// storer node types, which may sit behind a NAT: once a peer holds a
    /// relayed connection through the relay-client transport, it coordinates
    /// the simultaneous-open hole punch that upgrades the circuit to a direct
    /// connection. A bootnode is publicly reachable and never holds a relayed
    /// connection, so it carries no DCUtR. mDNS needs the local [`PeerId`], so
    /// the behaviour is built where the swarm's public key is available.
    pub(crate) fn from_config(
        config: &impl SwarmNetworkConfig,
        local_peer_id: PeerId,
        node_type: SwarmNodeType,
    ) -> Self {
        let autonat = config.autonat_enabled();
        let relay_server = matches!(node_type, SwarmNodeType::Bootnode | SwarmNodeType::Storer);
        let dcutr = matches!(node_type, SwarmNodeType::Client | SwarmNodeType::Storer);
        Self {
            autonat_client: Toggle::from(autonat.then(autonat::client::Behaviour::default)),
            autonat_server: Toggle::from(autonat.then(autonat::server::Behaviour::default)),
            relay_server: Toggle::from(
                relay_server
                    .then(|| relay::Behaviour::new(local_peer_id, relay::Config::default())),
            ),
            dcutr: Toggle::from(dcutr.then(|| dcutr::Behaviour::new(local_peer_id))),
            upnp: Toggle::from(config.upnp_enabled().then(upnp::tokio::Behaviour::default)),
            mdns: build_mdns_toggle(config.mdns_enabled(), local_peer_id),
        }
    }
}

/// Events emitted by [`NatBehaviour`].
pub(crate) enum NatEvent {
    AutonatClient(autonat::client::Event),
    AutonatServer(autonat::server::Event),
    Relay(relay::Event),
    Dcutr(dcutr::Event),
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

impl From<relay::Event> for NatEvent {
    fn from(event: relay::Event) -> Self {
        NatEvent::Relay(event)
    }
}

impl From<dcutr::Event> for NatEvent {
    fn from(event: dcutr::Event) -> Self {
        NatEvent::Dcutr(event)
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
        NatEvent::Relay(event) => handle_relay_event(event),
        NatEvent::Dcutr(event) => handle_dcutr_event(event),
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

/// Handle a circuit relay v2 server event.
///
/// The relay behaviour enforces its own reservation and circuit bounds; the
/// lifecycle is surfaced here as debug logs and counters only. Deprecated
/// failure variants are logged inside libp2p and matched by the wildcard arm.
fn handle_relay_event(event: relay::Event) {
    match event {
        relay::Event::ReservationReqAccepted {
            src_peer_id,
            renewed,
        } => {
            metrics::counter!("swarm.relay.reservations_accepted").increment(1);
            debug!(%src_peer_id, renewed, "Relay reservation accepted");
        }
        relay::Event::ReservationReqDenied {
            src_peer_id,
            status,
        } => {
            metrics::counter!("swarm.relay.reservations_denied").increment(1);
            debug!(%src_peer_id, ?status, "Relay reservation denied");
        }
        relay::Event::ReservationTimedOut { src_peer_id } => {
            debug!(%src_peer_id, "Relay reservation timed out");
        }
        relay::Event::ReservationClosed { src_peer_id } => {
            debug!(%src_peer_id, "Relay reservation closed");
        }
        relay::Event::CircuitReqAccepted {
            src_peer_id,
            dst_peer_id,
        } => {
            metrics::counter!("swarm.relay.circuits_accepted").increment(1);
            debug!(%src_peer_id, %dst_peer_id, "Relay circuit established");
        }
        relay::Event::CircuitReqDenied {
            src_peer_id,
            dst_peer_id,
            status,
        } => {
            metrics::counter!("swarm.relay.circuits_denied").increment(1);
            debug!(%src_peer_id, %dst_peer_id, ?status, "Relay circuit denied");
        }
        relay::Event::CircuitClosed {
            src_peer_id,
            dst_peer_id,
            error,
        } => {
            metrics::counter!("swarm.relay.circuits_closed").increment(1);
            debug!(%src_peer_id, %dst_peer_id, ?error, "Relay circuit closed");
        }
        _ => {}
    }
}

/// Handle a DCUtR event: the outcome of a hole punch attempted after a peer
/// connected over a relayed circuit, surfaced as counters and debug logs only.
/// On success the direct connection replaces the circuit for new substreams;
/// on failure the relayed connection stays up, so no peer state changes here.
fn handle_dcutr_event(event: dcutr::Event) {
    match event.result {
        Ok(connection_id) => {
            metrics::counter!("swarm.dcutr.holepunch_success").increment(1);
            debug!(
                remote_peer_id = %event.remote_peer_id,
                %connection_id,
                "Hole punch succeeded, direct connection established"
            );
        }
        Err(error) => {
            metrics::counter!("swarm.dcutr.holepunch_failure").increment(1);
            debug!(
                remote_peer_id = %event.remote_peer_id,
                %error,
                "Hole punch failed, keeping the relayed connection"
            );
        }
    }
}

/// Handle a circuit relay v2 client event: reservation and relayed-circuit
/// lifecycle, surfaced as counters and debug logs only.
pub(crate) fn handle_relay_client_event(event: RelayClientEvent) {
    match event {
        relay::client::Event::ReservationReqAccepted {
            relay_peer_id,
            renewal,
            ..
        } => {
            metrics::counter!("swarm.relay_client.reservations_accepted").increment(1);
            debug!(%relay_peer_id, renewal, "Relay reservation accepted");
        }
        relay::client::Event::OutboundCircuitEstablished { relay_peer_id, .. } => {
            metrics::counter!("swarm.relay_client.outbound_circuits").increment(1);
            debug!(%relay_peer_id, "Outbound relay circuit established");
        }
        relay::client::Event::InboundCircuitEstablished { src_peer_id, .. } => {
            metrics::counter!("swarm.relay_client.inbound_circuits").increment(1);
            debug!(%src_peer_id, "Inbound relay circuit established");
        }
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
    use std::time::Duration;

    use libp2p::identity::Keypair;

    use super::*;

    fn random_peer_id() -> PeerId {
        Keypair::generate_ed25519().public().to_peer_id()
    }

    /// Minimal network config for toggle assembly. mDNS is disabled so the
    /// test never binds a multicast socket.
    #[derive(Default)]
    struct TestConfig {
        addrs: Vec<Multiaddr>,
    }

    impl SwarmNetworkConfig for TestConfig {
        fn listen_addrs(&self) -> &[Multiaddr] {
            &self.addrs
        }
        fn bootnodes(&self) -> &[Multiaddr] {
            &self.addrs
        }
        fn discovery_enabled(&self) -> bool {
            true
        }
        fn max_peers(&self) -> usize {
            8
        }
        fn idle_timeout(&self) -> Duration {
            Duration::from_secs(30)
        }
        fn mdns_enabled(&self) -> bool {
            false
        }
    }

    #[test]
    fn relay_server_follows_node_type() {
        let config = TestConfig::default();
        for (node_type, enabled) in [
            (SwarmNodeType::Bootnode, true),
            (SwarmNodeType::Storer, true),
            (SwarmNodeType::Client, false),
        ] {
            let nat = NatBehaviour::from_config(&config, random_peer_id(), node_type);
            assert_eq!(
                nat.relay_server.is_enabled(),
                enabled,
                "relay server toggle for {node_type:?}"
            );
        }
    }

    #[test]
    fn dcutr_follows_node_type() {
        let config = TestConfig::default();
        for (node_type, enabled) in [
            (SwarmNodeType::Bootnode, false),
            (SwarmNodeType::Storer, true),
            (SwarmNodeType::Client, true),
        ] {
            let nat = NatBehaviour::from_config(&config, random_peer_id(), node_type);
            assert_eq!(
                nat.dcutr.is_enabled(),
                enabled,
                "dcutr toggle for {node_type:?}"
            );
        }
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
