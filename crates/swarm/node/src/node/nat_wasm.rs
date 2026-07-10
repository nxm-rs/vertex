//! NAT traversal and LAN discovery for the browser client.
//!
//! The browser client dials over websockets and never listens; there is no NAT
//! or LAN-discovery surface, and the `autonat`, `relay`, `upnp`, and `mdns`
//! libp2p features are dropped from the wasm dependency set. The dummy inner
//! behaviour keeps the composite shape and the call sites identical to the
//! native sibling module (`nat.rs`).

use std::convert::Infallible;

use libp2p::PeerId;
use libp2p::swarm::NetworkBehaviour;
use vertex_swarm_api::{SwarmIdentity, SwarmNetworkConfig};
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_topology::TopologyBehaviour;

/// No-op relay-client stand-in for the browser client. The browser swarm
/// assembly injects no relay transport yet (the relayed path arrives with the
/// WebTransport stack), so the composite carries an inert field on wasm.
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "RelayClientEvent")]
pub(crate) struct RelayClientBehaviour {
    inner: libp2p::swarm::dummy::Behaviour,
}

impl RelayClientBehaviour {
    pub(crate) fn new() -> Self {
        Self {
            inner: libp2p::swarm::dummy::Behaviour,
        }
    }
}

/// Uninhabited: the wasm relay-client stand-in never emits events.
pub(crate) enum RelayClientEvent {}

impl From<Infallible> for RelayClientEvent {
    fn from(event: Infallible) -> Self {
        match event {}
    }
}

/// Dispatch a relay-client event; statically unreachable in the browser.
pub(crate) fn handle_relay_client_event(event: RelayClientEvent) {
    match event {}
}

/// No-op NAT sub-behaviour for the browser client.
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "NatEvent")]
pub(crate) struct NatBehaviour {
    inner: libp2p::swarm::dummy::Behaviour,
}

impl NatBehaviour {
    /// Build the no-op NAT behaviour. The configuration, local [`PeerId`], and
    /// node type are accepted and ignored so the call site matches the native
    /// sibling.
    pub(crate) fn from_config(
        _config: &impl SwarmNetworkConfig,
        _local_peer_id: PeerId,
        _node_type: SwarmNodeType,
    ) -> Self {
        Self {
            inner: libp2p::swarm::dummy::Behaviour,
        }
    }
}

/// Uninhabited: the wasm NAT behaviour never emits events.
pub(crate) enum NatEvent {}

impl From<Infallible> for NatEvent {
    fn from(event: Infallible) -> Self {
        match event {}
    }
}

/// Dispatch a [`NatEvent`]; statically unreachable in the browser.
pub(crate) fn handle_nat_event<I: SwarmIdentity + Clone>(
    _local_peer_id: PeerId,
    _topology: &mut TopologyBehaviour<I>,
    event: NatEvent,
) {
    match event {}
}
