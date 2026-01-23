//! Network behaviour composition for SwarmNode.
//!
//! This module provides `NodeBehaviour` which combines:
//! - `identify`: Peer identification protocol
//! - `topology`: Handshake, hive, and pingpong protocols
//! - `client`: Pricing, retrieval, and pushsync protocols
//!
//! The behaviour itself is non-generic because libp2p's derive macro
//! doesn't easily support PhantomData. The generics are handled at the
//! [`SwarmNode`] level instead.

use std::sync::Arc;

use libp2p::{identify, identity::PublicKey, swarm::NetworkBehaviour};
use vertex_net_client::{
    BehaviourConfig as ClientBehaviourConfig, ClientEvent, SwarmClientBehaviour,
};
use vertex_net_topology::{
    BehaviourConfig as TopologyBehaviourConfig, SwarmTopologyBehaviour, TopologyEvent,
};
use vertex_node_types::NodeTypes;

/// Combined network behaviour for a SwarmNode.
///
/// This composes the topology and client behaviours into a single swarm behaviour.
/// Generic over `N: NodeTypes` to support different node configurations.
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "NodeEvent")]
pub struct NodeBehaviour<N: NodeTypes> {
    /// Identify protocol - exchange peer info.
    pub identify: identify::Behaviour,

    /// Topology behaviour - handshake, hive, ping.
    pub topology: SwarmTopologyBehaviour<N>,

    /// Client behaviour - pricing, retrieval, pushsync.
    pub client: SwarmClientBehaviour,
}

impl<N: NodeTypes> NodeBehaviour<N> {
    /// Create a new node behaviour.
    pub fn new(local_public_key: PublicKey, identity: Arc<N::Identity>) -> Self {
        Self {
            identify: identify::Behaviour::new(identify::Config::new(
                "/vertex/1.0.0".to_string(),
                local_public_key,
            )),
            topology: SwarmTopologyBehaviour::new(identity, TopologyBehaviourConfig::default()),
            client: SwarmClientBehaviour::new(ClientBehaviourConfig::default()),
        }
    }
}

/// Events from the combined node behaviour.
#[derive(Debug)]
pub enum NodeEvent {
    /// Identify protocol event.
    Identify(identify::Event),
    /// Topology event (peer ready, disconnected, discovered).
    Topology(TopologyEvent),
    /// Client event (pricing, retrieval, pushsync).
    Client(ClientEvent),
}

impl From<identify::Event> for NodeEvent {
    fn from(event: identify::Event) -> Self {
        NodeEvent::Identify(event)
    }
}

impl From<TopologyEvent> for NodeEvent {
    fn from(event: TopologyEvent) -> Self {
        NodeEvent::Topology(event)
    }
}

impl From<ClientEvent> for NodeEvent {
    fn from(event: ClientEvent) -> Self {
        NodeEvent::Client(event)
    }
}
