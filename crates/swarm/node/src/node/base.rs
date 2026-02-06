//! Shared infrastructure for all node types.

use eyre::Result;
use libp2p::{Multiaddr, PeerId, Swarm, swarm::NetworkBehaviour, swarm::SwarmEvent};
use nectar_primitives::SwarmAddress;
use tracing::{debug, info, warn};
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_topology::TopologyHandle;

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

    pub fn start_listening(&mut self) -> Result<()> {
        for addr in &self.listen_addrs {
            match self.swarm.listen_on(addr.clone()) {
                Ok(_) => info!(%addr, "Listening on address"),
                Err(e) => warn!(%addr, %e, "Failed to listen on address"),
            }
        }
        Ok(())
    }

    /// Handle common swarm events. Returns `Some` for behaviour events needing node-specific handling.
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
