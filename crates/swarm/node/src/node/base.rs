//! Shared infrastructure for all node types.

use std::sync::Arc;

use eyre::Result;
use libp2p::{Multiaddr, PeerId, Swarm, swarm::NetworkBehaviour, swarm::SwarmEvent};
use nectar_primitives::SwarmAddress;
use tracing::{debug, info, trace, warn};
use vertex_swarm_api::{SwarmIdentity, SwarmNodeTypes, SwarmTopology};
use vertex_swarm_kademlia::KademliaTopology;
use vertex_swarm_peermanager::{AddressManager, DiscoverySender, PeerManager};
use vertex_swarm_primitives::OverlayAddress;
use vertex_swarm_topology::{BootnodeConnector, TopologyEvent, is_dnsaddr};

/// Base node with shared state for [`BootNode`](super::BootNode),
/// [`ClientNode`](super::ClientNode), and [`StorerNode`](super::StorerNode).
pub struct BaseNode<N: SwarmNodeTypes, B: NetworkBehaviour> {
    pub(crate) swarm: Swarm<B>,
    pub(crate) identity: N::Identity,
    pub(crate) peer_manager: Arc<PeerManager>,
    pub(crate) address_manager: Option<Arc<AddressManager>>,
    pub(crate) kademlia: Arc<KademliaTopology<N::Identity>>,
    pub(crate) bootnode_connector: BootnodeConnector,
    pub(crate) listen_addrs: Vec<Multiaddr>,
    pub(crate) discovery_tx: DiscoverySender,
}

impl<N: SwarmNodeTypes, B: NetworkBehaviour> BaseNode<N, B> {
    pub fn local_peer_id(&self) -> &PeerId {
        self.swarm.local_peer_id()
    }

    pub fn overlay_address(&self) -> SwarmAddress {
        self.identity.overlay_address()
    }

    pub fn identity(&self) -> &N::Identity {
        &self.identity
    }

    pub fn peer_manager(&self) -> &Arc<PeerManager> {
        &self.peer_manager
    }

    pub fn kademlia_topology(&self) -> &Arc<KademliaTopology<N::Identity>> {
        &self.kademlia
    }

    pub fn connected_peers(&self) -> usize {
        self.swarm.connected_peers().count()
    }

    pub fn is_connected(&self) -> bool {
        self.connected_peers() > 0
    }

    pub fn start_listening(&mut self) -> Result<()> {
        for addr in &self.listen_addrs {
            match self.swarm.listen_on(addr.clone()) {
                Ok(_) => info!(%addr, "Listening on address"),
                Err(e) => warn!(%addr, %e, "Failed to listen on address"),
            }
        }
        Ok(())
    }

    /// Connect to bootnodes. DNS addresses are resolved by libp2p's DNS transport.
    pub async fn connect_bootnodes(&mut self) -> Result<usize> {
        let bootnodes = self.bootnode_connector.shuffled_bootnodes();

        if bootnodes.is_empty() {
            warn!("No bootnodes configured");
            return Ok(0);
        }

        info!(count = bootnodes.len(), "Connecting to bootnodes...");

        let mut connected = 0;
        let min_connections = self.bootnode_connector.min_connections();

        for bootnode in bootnodes {
            if connected >= min_connections {
                info!(connected, "Reached minimum bootnode connections");
                break;
            }

            let is_dns = is_dnsaddr(&bootnode);
            info!(
                %bootnode,
                is_dnsaddr = is_dns,
                "Dialing bootnode{}",
                if is_dns { " (dnsaddr will be resolved)" } else { "" }
            );

            match self.swarm.dial(bootnode.clone()) {
                Ok(_) => {
                    debug!(%bootnode, "Dial initiated");
                    connected += 1;
                }
                Err(e) => {
                    warn!(%bootnode, %e, "Failed to dial bootnode");
                }
            }
        }

        Ok(connected)
    }

    /// Handle common swarm events. Returns `Some` for behaviour events needing node-specific handling.
    pub(crate) fn handle_swarm_event_common<E>(
        &mut self,
        event: SwarmEvent<E>,
    ) -> Option<SwarmEvent<E>> {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                info!(%address, "New listen address");
                if let Some(mgr) = &self.address_manager {
                    mgr.on_new_listen_addr(address.clone());
                    debug!(
                        listen_count = mgr.listen_addrs().len(),
                        "AddressManager tracking listen addresses"
                    );
                }
                None
            }
            SwarmEvent::ExpiredListenAddr { address, .. } => {
                info!(%address, "Expired listen address");
                if let Some(mgr) = &self.address_manager {
                    mgr.on_expired_listen_addr(&address);
                }
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
                info!(
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
                if let Some(peer_id) = peer_id {
                    warn!(%peer_id, %error, "Outgoing connection error");
                    // Connection failed before handshake, so no peer_manager mapping exists.
                    // Kademlia gets updated via dial_connection_candidates tracking.
                } else {
                    warn!(%error, "Outgoing connection error (unknown peer)");
                }
                None
            }
            SwarmEvent::Behaviour(_) => Some(event),
            _ => None,
        }
    }

    /// Handle topology event. Callback is invoked on peer authentication.
    ///
    /// The behaviour has already updated the PeerManager; we just need to
    /// update kademlia and invoke the callback.
    pub(crate) fn handle_topology_event(
        &mut self,
        event: TopologyEvent,
        on_peer_authenticated: impl FnOnce(&mut Self, OverlayAddress, bool),
    ) {
        match event {
            TopologyEvent::PeerAuthenticated {
                peer,
                is_full_node,
                welcome_message: _,
            } => {
                let overlay = OverlayAddress::from(*peer.overlay());
                debug!(%overlay, %is_full_node, "Peer authenticated");

                self.kademlia.connected(overlay);
                on_peer_authenticated(self, overlay, is_full_node);
            }
            TopologyEvent::PeerConnectionClosed { overlay } => {
                debug!(%overlay, "Peer disconnected");
                self.kademlia.disconnected(&overlay);
            }
            TopologyEvent::HivePeersReceived { from, peers } => {
                debug!(%from, count = peers.len(), "Received peers via hive");

                // Send all peers to discovery channel for persistence (includes non-dialable)
                for peer in &peers {
                    if let Err(e) = self.discovery_tx.send(peer.clone()) {
                        trace!(error = %e, "discovery channel full or closed");
                    }
                }

                // Store only dialable peers and get their overlays for Kademlia
                let stored_overlays = self.peer_manager.store_hive_peers_batch(peers);

                if !stored_overlays.is_empty() {
                    self.kademlia.add_peers(&stored_overlays);
                    self.kademlia.evaluate_connections();
                    self.dial_connection_candidates();
                }
            }
            TopologyEvent::DialFailed { address, error } => {
                warn!(%address, %error, "Dial failed");
            }
            TopologyEvent::DepthChanged { new_depth } => {
                info!(%new_depth, "Network depth changed");
            }
        }
    }

    pub(crate) fn dial_connection_candidates(&mut self) {
        let candidates = self.kademlia.peers_to_connect();
        let dialable = self.peer_manager.filter_dialable_candidates(&candidates);

        for (overlay, multiaddrs) in dialable {
            let original_count = multiaddrs.len();

            // Filter multiaddrs by IP version compatibility if we have an address manager
            let compatible_addrs: Vec<_> = match &self.address_manager {
                Some(mgr) => mgr.filter_dialable_addrs(&multiaddrs).cloned().collect(),
                None => multiaddrs, // No address manager, use all addresses
            };

            if compatible_addrs.is_empty() {
                trace!(
                    %overlay,
                    addr_count = original_count,
                    "No IP-compatible multiaddrs for peer"
                );
                continue;
            }

            let Some((addr, peer_id)) = compatible_addrs.iter().find_map(|addr| {
                addr.iter().find_map(|p| {
                    if let libp2p::multiaddr::Protocol::P2p(id) = p {
                        Some((addr.clone(), id))
                    } else {
                        None
                    }
                })
            }) else {
                debug!(%overlay, "No multiaddr with peer_id found");
                continue;
            };

            if self.swarm.is_connected(&peer_id) {
                continue;
            }

            debug!(%overlay, %addr, %peer_id, "Dialing discovered peer");

            if !self.peer_manager.start_connecting(overlay) {
                continue;
            }
            self.kademlia.start_connecting(overlay);

            if let Err(e) = self.swarm.dial(addr.clone()) {
                debug!(%overlay, %addr, %e, "Failed to dial discovered peer");
                self.peer_manager.connection_failed(&overlay);
            }
        }
    }
}
