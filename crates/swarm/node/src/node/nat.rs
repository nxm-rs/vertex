//! NAT traversal and LAN discovery for native node types.
//!
//! [`NatBehaviour`] composes AutoNAT v2 (client + server), UPnP, and mDNS into
//! a single sub-behaviour so the node composites carry one platform-neutral
//! field. The browser client dials over websockets and never listens, so it
//! has no NAT or LAN-discovery surface; the wasm sibling module (`nat_wasm.rs`)
//! exposes the same item names and signatures over a no-op behaviour.

use libp2p::autonat::v2 as autonat;
use libp2p::mdns;
use libp2p::multiaddr::Protocol;
use libp2p::swarm::NetworkBehaviour;
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::upnp;
use libp2p::{Multiaddr, PeerId};
use tracing::{debug, info, warn};
use vertex_swarm_api::{SwarmIdentity, SwarmNetworkConfig};
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
    autonat_server: Toggle<autonat::server::Behaviour>,
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
            autonat_server: Toggle::from(autonat.then(autonat::server::Behaviour::default)),
            upnp: Toggle::from(config.upnp_enabled().then(upnp::tokio::Behaviour::default)),
            mdns: build_mdns_toggle(config.mdns_enabled(), local_peer_id),
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
    use libp2p::identity::Keypair;

    use super::*;

    fn random_peer_id() -> PeerId {
        Keypair::generate_ed25519().public().to_peer_id()
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
