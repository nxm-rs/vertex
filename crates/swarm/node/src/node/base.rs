//! Shared infrastructure for all node types.

use eyre::Result;
use libp2p::connection_limits::{self, ConnectionLimits};
use libp2p::{Multiaddr, PeerId, Swarm, swarm::NetworkBehaviour, swarm::SwarmEvent};
use nectar_primitives::SwarmAddress;
use tracing::{debug, info, trace, warn};
use vertex_swarm_api::{SwarmIdentity, SwarmNetworkConfig};
use vertex_swarm_net_identify as identify;
use vertex_swarm_topology::TopologyHandle;

/// Maximum half-open inbound connections (accepted at the transport but not
/// yet upgraded). Bounds the resources an accept flood can pin before any
/// protocol negotiation happens.
const MAX_PENDING_INCOMING: u32 = 64;

/// Maximum in-flight outbound dials. The topology dialer paces its own dials
/// at 32 concurrent attempts (`vertex-net-dialer`); the transport cap sits at
/// twice that to leave room for bootnode dials, mDNS-discovered LAN dials,
/// and operator-issued dial commands that do not flow through the tracker.
const MAX_PENDING_OUTGOING: u32 = 64;

/// Maximum established connections per peer. One connection per peer is the
/// norm on this network; 2 tolerates the simultaneous-open race where both
/// ends dial each other and a duplicate exists briefly. The topology layer
/// detects and closes duplicate connections itself.
const MAX_ESTABLISHED_PER_PEER: u32 = 2;

/// Build the transport-level connection-limits behaviour from the network
/// configuration.
///
/// The total established cap comes from `--network.max-peers` and is a
/// resource backstop (file descriptors, memory, bandwidth), enforced by the
/// swarm independently of kademlia bin logic. Topology keeps full ownership
/// of connection composition: per-bin targets, saturation, inbound ceilings,
/// and trimming. The transport cap only bounds how many connections can
/// exist at all, so it must sit above topology's own steady-state total
/// (see the `--network.max-peers` default rationale in the args module).
pub(crate) fn build_connection_limits(
    config: &impl SwarmNetworkConfig,
) -> connection_limits::Behaviour {
    let max_established = u32::try_from(config.max_peers()).unwrap_or(u32::MAX);
    connection_limits::Behaviour::new(
        ConnectionLimits::default()
            .with_max_established(Some(max_established))
            .with_max_established_per_peer(Some(MAX_ESTABLISHED_PER_PEER))
            .with_max_pending_incoming(Some(MAX_PENDING_INCOMING))
            .with_max_pending_outgoing(Some(MAX_PENDING_OUTGOING)),
    )
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
#[allow(clippy::expect_used)]
mod tests {
    use std::time::Duration;

    use libp2p::swarm::{ListenError, SwarmEvent};
    use libp2p_swarm_test::SwarmExt as _;

    use super::*;

    /// Minimal network configuration carrying only the connection cap.
    struct CapConfig {
        max_peers: usize,
    }

    impl SwarmNetworkConfig for CapConfig {
        fn listen_addrs(&self) -> &[Multiaddr] {
            &[]
        }
        fn bootnodes(&self) -> &[Multiaddr] {
            &[]
        }
        fn discovery_enabled(&self) -> bool {
            false
        }
        fn max_peers(&self) -> usize {
            self.max_peers
        }
        fn idle_timeout(&self) -> Duration {
            Duration::from_secs(30)
        }
    }

    /// Behaviour carrying only the connection-limits backstop, mirroring its
    /// position as the admission gate in the node-type compositions.
    #[derive(libp2p::swarm::NetworkBehaviour)]
    struct LimitsOnly {
        limits: connection_limits::Behaviour,
    }

    fn limits_swarm(max_peers: usize) -> Swarm<LimitsOnly> {
        Swarm::new_ephemeral_tokio(|_| LimitsOnly {
            limits: build_connection_limits(&CapConfig { max_peers }),
        })
    }

    /// `--network.max-peers` flows into the transport cap: with a cap of 1,
    /// the first connection is admitted and the second inbound connection is
    /// denied with a connection-limits `Exceeded` cause.
    #[tokio::test]
    async fn max_peers_caps_established_connections() {
        let mut listener = limits_swarm(1);
        let mut first = limits_swarm(16);
        let mut second = limits_swarm(16);

        listener.listen().with_memory_addr_external().await;
        first.connect(&mut listener).await;

        let listen_addr = listener
            .external_addresses()
            .next()
            .cloned()
            .expect("listener has an external address");
        second.dial(listen_addr).expect("dial is initiated");

        let denial = async {
            loop {
                tokio::select! {
                    event = listener.next_swarm_event() => {
                        if let SwarmEvent::IncomingConnectionError {
                            error: ListenError::Denied { cause },
                            ..
                        } = event
                        {
                            return cause;
                        }
                    }
                    _ = first.next_swarm_event() => {}
                    _ = second.next_swarm_event() => {}
                }
            }
        };
        let cause = vertex_tasks::time::timeout(Duration::from_secs(10), denial)
            .await
            .expect("listener denies the second connection");

        let exceeded = cause
            .downcast::<libp2p::connection_limits::Exceeded>()
            .expect("denial cause is a connection-limits Exceeded");
        assert_eq!(exceeded.limit(), 1, "the cap comes from max_peers");
    }
}
