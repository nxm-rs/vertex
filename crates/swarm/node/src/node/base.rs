//! Shared infrastructure for all node types.

use eyre::Result;
use libp2p::autonat::v2 as autonat;
use libp2p::mdns;
use libp2p::multiaddr::Protocol;
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::upnp;
use libp2p::{Multiaddr, PeerId, Swarm, swarm::NetworkBehaviour, swarm::SwarmEvent};
use nectar_primitives::SwarmAddress;
use tracing::{debug, info, trace, warn};
use vertex_swarm_api::{SwarmIdentity, SwarmNetworkConfig};
use vertex_swarm_net_identify as identify;
use vertex_swarm_topology::{TopologyBehaviour, TopologyCommand, TopologyHandle};

/// Optional NAT-traversal behaviours shared by every node type.
///
/// AutoNAT v2 (client + server) and UPnP are wired as top-level siblings of
/// identify so the libp2p swarm propagates verified external addresses between
/// them automatically. Each is wrapped in a [`Toggle`] so an operator can
/// disable it without changing the behaviour type.
pub(crate) struct NatBehaviours {
    pub(crate) autonat_client: Toggle<autonat::client::Behaviour>,
    pub(crate) autonat_server: Toggle<autonat::server::Behaviour>,
    pub(crate) upnp: Toggle<upnp::tokio::Behaviour>,
    /// Whether mDNS discovery is enabled. The behaviour itself is built in the
    /// node `from_parts`, where the local [`PeerId`] is available.
    pub(crate) mdns_enabled: bool,
}

impl NatBehaviours {
    /// Build the NAT behaviours from a network configuration.
    ///
    /// AutoNAT v2 and mDNS are enabled by default for every node type; UPnP is
    /// opt-in.
    pub(crate) fn from_config(config: &impl SwarmNetworkConfig) -> Self {
        let autonat = config.autonat_enabled();
        Self {
            autonat_client: Toggle::from(autonat.then(autonat::client::Behaviour::default)),
            autonat_server: Toggle::from(autonat.then(autonat::server::Behaviour::default)),
            upnp: Toggle::from(config.upnp_enabled().then(upnp::tokio::Behaviour::default)),
            mdns_enabled: config.mdns_enabled(),
        }
    }
}

/// Build the mDNS discovery behaviour as a [`Toggle`].
///
/// mDNS construction is fallible (it binds a multicast socket) and needs the
/// local [`PeerId`], so it is built in the node behaviour `from_parts` rather
/// than alongside the config-only NAT behaviours. A bind failure never aborts
/// node startup: the behaviour is logged and left disabled.
///
/// A future browser/wasm client would gate this off, since mDNS has no
/// `wasm32-unknown-unknown` transport; the node crate is not in the wasm cone.
pub(crate) fn build_mdns_toggle(enabled: bool, peer_id: PeerId) -> Toggle<mdns::tokio::Behaviour> {
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
pub(crate) fn mdns_dial_addr(
    local_peer_id: &PeerId,
    peer_id: PeerId,
    addr: Multiaddr,
) -> Option<Multiaddr> {
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
pub(crate) fn handle_mdns_event<I: SwarmIdentity + Clone>(
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
pub(crate) fn handle_autonat_server_event<I: SwarmIdentity + Clone>(
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
pub(crate) fn handle_autonat_client_event(event: autonat::client::Event) {
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
pub(crate) fn handle_upnp_event(event: upnp::Event) {
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

/// Base node with shared state for [`BootNode`](super::BootNode),
/// [`ClientNode`](super::ClientNode), and [`StorerNode`](super::StorerNode).
pub struct BaseNode<I: SwarmIdentity, B: NetworkBehaviour> {
    pub(crate) swarm: Swarm<B>,
    pub(crate) identity: I,
    pub(crate) listen_addrs: Vec<Multiaddr>,
    pub(crate) topology_handle: TopologyHandle<I>,
}

impl<I: SwarmIdentity, B: NetworkBehaviour> BaseNode<I, B> {
    pub fn local_peer_id(&self) -> &PeerId {
        self.swarm.local_peer_id()
    }

    pub fn overlay_address(&self) -> SwarmAddress {
        self.identity.overlay_address()
    }

    pub fn identity(&self) -> &I {
        &self.identity
    }

    /// Get the unified topology handle for queries.
    pub fn topology_handle(&self) -> &TopologyHandle<I> {
        &self.topology_handle
    }

    pub fn connected_peers(&self) -> usize {
        self.swarm.connected_peers().count()
    }

    pub fn is_connected(&self) -> bool {
        self.connected_peers() > 0
    }

    #[must_use = "listen failures should be checked"]
    pub fn start_listening(&mut self) -> Result<()> {
        for addr in &self.listen_addrs {
            match self.swarm.listen_on(addr.clone()) {
                Ok(_) => info!(%addr, "Listening on address"),
                Err(e) => warn!(%addr, %e, "Failed to listen on address"),
            }
        }
        Ok(())
    }

    /// Handle common swarm events. Returns `Some(Behaviour)` for behaviour events.
    ///
    /// Listen address events (NewListenAddr, ExpiredListenAddr) are handled by TopologyBehaviour.
    pub(crate) fn handle_swarm_event_common<E>(
        &mut self,
        event: SwarmEvent<E>,
    ) -> Option<SwarmEvent<E>> {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                info!(%address, "New listen address");
                // AddressManager tracking handled by TopologyBehaviour
                None
            }
            SwarmEvent::ExpiredListenAddr { address, .. } => {
                info!(%address, "Expired listen address");
                // AddressManager tracking handled by TopologyBehaviour
                None
            }
            SwarmEvent::ConnectionEstablished {
                peer_id,
                endpoint,
                num_established,
                ..
            } => {
                debug!(
                    %peer_id,
                    endpoint = %endpoint.get_remote_address(),
                    num_established,
                    "Connection established"
                );
                None
            }
            SwarmEvent::ConnectionClosed {
                peer_id,
                cause,
                num_established,
                ..
            } => {
                debug!(
                    %peer_id,
                    num_established,
                    cause = ?cause,
                    "Connection closed"
                );
                None
            }
            SwarmEvent::IncomingConnection {
                local_addr,
                send_back_addr,
                ..
            } => {
                debug!(%local_addr, %send_back_addr, "Incoming connection");
                None
            }
            SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                // Dial retry is now handled by TopologyBehaviour via FromSwarm::DialFailure
                if let Some(peer_id) = peer_id {
                    debug!(%peer_id, %error, "Outgoing connection error");
                } else {
                    debug!(%error, "Outgoing connection error (unknown peer)");
                }
                None
            }
            SwarmEvent::Behaviour(_) => Some(event),
            _ => None,
        }
    }
}

/// Handle an identify event: push the peer-observed address back, and let the
/// identify behaviour surface it as an AutoNAT external-address candidate.
///
/// Shared between [`BootNode`](super::BootNode) and [`ClientNode`](super::ClientNode).
pub(crate) fn handle_identify_event(identify: &mut identify::Behaviour, event: identify::Event) {
    match event {
        identify::Event::Received { peer_id, info, .. } => {
            debug!(
                %peer_id,
                protocol_version = %info.protocol_version,
                agent_version = %info.agent_version,
                observed_addr = %info.observed_addr,
                "Received identify info"
            );

            // The observed address flows to the AutoNAT v2 client as an
            // external-address candidate (emitted inside the identify
            // behaviour) to be verified by dial-back; we do not treat it as
            // proof of public connectivity here. Self-reachability is set only
            // by a verified external address or an inbound handshake whose
            // observed address is public (see the topology handshake handler).
            if !info.observed_addr.is_empty() {
                identify.push_with_addresses(peer_id, vec![info.observed_addr]);
            }
        }
        identify::Event::Sent { peer_id, .. } => {
            trace!(%peer_id, "Sent identify info");
        }
        identify::Event::Pushed { peer_id, .. } => {
            debug!(%peer_id, "Pushed identify info");
        }
        identify::Event::Error { peer_id, error, .. } => {
            warn!(%peer_id, %error, "Identify error");
        }
    }
}

#[cfg(test)]
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
