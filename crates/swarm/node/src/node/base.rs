//! Shared infrastructure for all node types.

use eyre::Result;
use libp2p::{Multiaddr, PeerId, Swarm, swarm::NetworkBehaviour, swarm::SwarmEvent};
use nectar_primitives::SwarmAddress;
use tracing::{debug, info, trace, warn};
use vertex_swarm_api::{SwarmIdentity, SwarmTopologyStats};
use vertex_swarm_net_identify as identify;
use vertex_swarm_topology::{TopologyBehaviour, TopologyCommand, TopologyHandle};

/// The base sub-behaviours (topology + identify) shared by every node type.
///
/// Implemented by each composed behaviour so [`BaseNode`] can provide common
/// event handlers generically.
pub(crate) trait BaseBehaviour<I: SwarmIdentity + Clone> {
    fn topology(&self) -> &TopologyBehaviour<I>;
    fn topology_mut(&mut self) -> &mut TopologyBehaviour<I>;
    fn identify_mut(&mut self) -> &mut identify::Behaviour;
}

/// Base node with shared state for [`BootNode`](super::BootNode),
/// [`ClientNode`](super::ClientNode), and [`StorerNode`](super::StorerNode).
pub struct BaseNode<I: SwarmIdentity, B: NetworkBehaviour> {
    pub(crate) swarm: Swarm<B>,
    pub(crate) identity: I,
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

    /// Connected peer count from topology metrics (O(1) via atomic counters).
    pub fn connected_peers(&self) -> usize {
        self.topology_handle.connected_peers_count()
    }

    pub fn is_connected(&self) -> bool {
        self.topology_handle.connection_registry().active_count() > 0
    }

    /// Start listening on the given addresses.
    ///
    /// Addresses are consumed here rather than stored, as live address tracking
    /// is handled by `TopologyBehaviour` via libp2p `FromSwarm` events.
    #[must_use = "listen failures should be checked"]
    pub fn start_listening(&mut self, addrs: &[Multiaddr]) -> Result<()> {
        for addr in addrs {
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

#[allow(private_bounds)]
impl<I: SwarmIdentity + Clone, B: NetworkBehaviour + BaseBehaviour<I>> BaseNode<I, B> {
    /// Handle an identify event by recording observed addresses and pushing them back.
    pub(crate) fn handle_identify_event(&mut self, event: identify::Event) {
        match event {
            identify::Event::Received { peer_id, info, .. } => {
                debug!(
                    %peer_id,
                    protocol_version = %info.protocol_version,
                    agent_version = %info.agent_version,
                    observed_addr = %info.observed_addr,
                    "Received identify info"
                );

                if !info.observed_addr.is_empty() {
                    let behaviour = self.swarm.behaviour_mut();
                    behaviour.topology().on_observed_addr(&info.observed_addr);
                    behaviour
                        .identify_mut()
                        .push_with_addresses(peer_id, vec![info.observed_addr]);
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

    /// Forward a topology command to the behaviour.
    pub(crate) fn topology_command(&mut self, command: TopologyCommand) {
        self.swarm.behaviour_mut().topology_mut().on_command(command);
    }

    /// Save peers via topology command (shutdown helper).
    pub(crate) fn save_peers(&mut self) {
        self.topology_command(TopologyCommand::SavePeers);
    }
}
