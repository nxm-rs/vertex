//! Combined protocol upgrades for topology handler.
//!
//! This module provides multi-protocol support for the topology handler,
//! combining handshake, hive, and pingpong into a single `InboundUpgrade`.
//!
//! # Architecture
//!
//! The topology handler needs to accept multiple inbound protocols:
//! - Handshake: Must complete before other protocols
//! - Hive: Peer discovery gossip (after handshake)
//! - Pingpong: Connection liveness (after handshake)
//!
//! We use a custom `TopologyInboundUpgrade` that implements `UpgradeInfo`
//! with all protocol names and dispatches based on the negotiated protocol.

use std::sync::Arc;

use futures::future::BoxFuture;
use libp2p::{InboundUpgrade, Multiaddr, OutboundUpgrade, PeerId, Stream, core::UpgradeInfo};
use thiserror::Error;
use tracing::debug;
use vertex_net_handshake::{
    HandshakeError, HandshakeInfo, HandshakeProtocol, PROTOCOL as HANDSHAKE_PROTOCOL,
};
use vertex_net_headers::ProtocolError;
use vertex_net_hive::{HiveOutboundProtocol, PROTOCOL_NAME as HIVE_PROTOCOL};
use vertex_net_pingpong::{
    PROTOCOL_NAME as PINGPONG_PROTOCOL, PingpongInboundProtocol, PingpongOutboundProtocol, Pong,
};
use vertex_swarm_api::{SwarmIdentity, SwarmNodeTypes};
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peermanager::AddressManager;

// ============================================================================
// Error Types
// ============================================================================

/// Errors from topology protocol upgrades.
#[derive(Debug, Error)]
pub enum TopologyUpgradeError {
    /// Handshake protocol error.
    #[error("handshake error: {0}")]
    Handshake(#[from] HandshakeError),

    /// Hive protocol error.
    #[error("hive error: {0}")]
    Hive(#[source] ProtocolError),

    /// Pingpong protocol error.
    #[error("pingpong error: {0}")]
    Pingpong(#[source] ProtocolError),

    /// Unknown protocol negotiated.
    #[error("unknown protocol: {0}")]
    UnknownProtocol(String),
}

// ============================================================================
// Inbound Protocol
// ============================================================================

/// Output from a topology inbound upgrade.
pub enum TopologyInboundOutput {
    /// Handshake completed successfully.
    Handshake(Box<HandshakeInfo>),
    /// Received validated peers via hive.
    Hive(Vec<SwarmPeer>),
    /// Responded to a ping.
    Pingpong,
}

/// Combined inbound upgrade for topology protocols.
///
/// Advertises handshake, hive, and pingpong protocols and dispatches
/// to the appropriate handler based on the negotiated protocol.
///
/// Generic over `N: SwarmNodeTypes` to support different node configurations.
#[derive(Clone)]
pub struct TopologyInboundUpgrade<N: SwarmNodeTypes> {
    identity: N::Identity,
    peer_id: PeerId,
    remote_addr: Multiaddr,
    address_manager: Option<Arc<AddressManager>>,
}

impl<N: SwarmNodeTypes> TopologyInboundUpgrade<N> {
    /// Create a new topology inbound upgrade.
    pub fn new(identity: N::Identity, peer_id: PeerId, remote_addr: Multiaddr) -> Self {
        Self {
            identity,
            peer_id,
            remote_addr,
            address_manager: None,
        }
    }

    /// Create a new topology inbound upgrade with address management.
    ///
    /// The AddressManager provides smart address selection based on the
    /// remote peer's network scope.
    pub fn with_address_manager(
        identity: N::Identity,
        peer_id: PeerId,
        remote_addr: Multiaddr,
        address_manager: Arc<AddressManager>,
    ) -> Self {
        Self {
            identity,
            peer_id,
            remote_addr,
            address_manager: Some(address_manager),
        }
    }
}

impl<N: SwarmNodeTypes> std::fmt::Debug for TopologyInboundUpgrade<N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TopologyInboundUpgrade")
            .field("peer_id", &self.peer_id)
            .field("remote_addr", &self.remote_addr)
            .finish_non_exhaustive()
    }
}

impl<N: SwarmNodeTypes> UpgradeInfo for TopologyInboundUpgrade<N> {
    type Info = &'static str;
    type InfoIter = std::vec::IntoIter<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        vec![HANDSHAKE_PROTOCOL, HIVE_PROTOCOL, PINGPONG_PROTOCOL].into_iter()
    }
}

impl<N: SwarmNodeTypes> InboundUpgrade<Stream> for TopologyInboundUpgrade<N> {
    type Output = TopologyInboundOutput;
    type Error = TopologyUpgradeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, socket: Stream, info: Self::Info) -> Self::Future {
        Box::pin(async move {
            match info {
                HANDSHAKE_PROTOCOL => {
                    // Get additional addresses from AddressManager if available
                    let additional_addrs = self
                        .address_manager
                        .as_ref()
                        .map(|mgr| mgr.addresses_for_peer(&self.remote_addr))
                        .unwrap_or_default();

                    debug!(
                        peer_id = %self.peer_id,
                        remote_addr = %self.remote_addr,
                        additional_addrs_count = additional_addrs.len(),
                        "Inbound handshake: selected addresses for peer"
                    );

                    let handshake = HandshakeProtocol::<N>::with_addrs(
                        self.identity,
                        self.peer_id,
                        self.remote_addr.clone(),
                        additional_addrs,
                    );
                    let result = handshake.upgrade_inbound(socket, info).await?;

                    // Report observed address to AddressManager if available
                    if let Some(mgr) = &self.address_manager {
                        debug!(
                            peer_id = %self.peer_id,
                            observed_addr = %result.observed_multiaddr(),
                            "Inbound handshake: reporting observed address"
                        );
                        mgr.on_observed_addr(
                            result.observed_multiaddr().clone(),
                            &self.remote_addr,
                        );
                    }

                    Ok(TopologyInboundOutput::Handshake(Box::new(result)))
                }
                HIVE_PROTOCOL => {
                    let hive = vertex_net_hive::inbound::<N>(self.identity.spec());
                    let validated = hive
                        .upgrade_inbound(socket, info)
                        .await
                        .map_err(TopologyUpgradeError::Hive)?;
                    Ok(TopologyInboundOutput::Hive(validated.peers))
                }
                PINGPONG_PROTOCOL => {
                    let pingpong: PingpongInboundProtocol = vertex_net_pingpong::inbound();
                    pingpong
                        .upgrade_inbound(socket, info)
                        .await
                        .map_err(TopologyUpgradeError::Pingpong)?;
                    Ok(TopologyInboundOutput::Pingpong)
                }
                other => Err(TopologyUpgradeError::UnknownProtocol(other.to_string())),
            }
        })
    }
}

// ============================================================================
// Outbound Protocol
// ============================================================================

/// Type of outbound request for topology.
#[derive(Debug, Clone)]
pub enum TopologyOutboundRequest {
    /// Initiate handshake.
    Handshake,
    /// Broadcast peers via hive.
    Hive(Vec<SwarmPeer>),
    /// Send a ping with greeting.
    Pingpong(String),
}

/// Output from a topology outbound upgrade.
pub enum TopologyOutboundOutput {
    /// Handshake completed.
    Handshake(Box<HandshakeInfo>),
    /// Hive broadcast completed.
    Hive,
    /// Pong received.
    Pingpong(Pong),
}

/// Combined outbound upgrade for topology protocols.
///
/// Unlike inbound, outbound requests know which protocol to use.
/// This enum wraps the specific request type.
///
/// Generic over `N: SwarmNodeTypes` to support different node configurations.
#[derive(Clone)]
pub struct TopologyOutboundUpgrade<N: SwarmNodeTypes> {
    identity: N::Identity,
    peer_id: PeerId,
    remote_addr: Multiaddr,
    request: TopologyOutboundRequest,
    address_manager: Option<Arc<AddressManager>>,
}

impl<N: SwarmNodeTypes> TopologyOutboundUpgrade<N> {
    /// Create a new handshake outbound upgrade.
    pub fn handshake(identity: N::Identity, peer_id: PeerId, remote_addr: Multiaddr) -> Self {
        Self {
            identity,
            peer_id,
            remote_addr,
            request: TopologyOutboundRequest::Handshake,
            address_manager: None,
        }
    }

    /// Create a new handshake outbound upgrade with address management.
    pub fn handshake_with_address_manager(
        identity: N::Identity,
        peer_id: PeerId,
        remote_addr: Multiaddr,
        address_manager: Arc<AddressManager>,
    ) -> Self {
        Self {
            identity,
            peer_id,
            remote_addr,
            request: TopologyOutboundRequest::Handshake,
            address_manager: Some(address_manager),
        }
    }

    /// Create a new hive outbound upgrade.
    pub fn hive(
        identity: N::Identity,
        peer_id: PeerId,
        remote_addr: Multiaddr,
        peers: Vec<SwarmPeer>,
    ) -> Self {
        Self {
            identity,
            peer_id,
            remote_addr,
            request: TopologyOutboundRequest::Hive(peers),
            address_manager: None,
        }
    }

    /// Create a new pingpong outbound upgrade.
    pub fn pingpong(
        identity: N::Identity,
        peer_id: PeerId,
        remote_addr: Multiaddr,
        greeting: String,
    ) -> Self {
        Self {
            identity,
            peer_id,
            remote_addr,
            request: TopologyOutboundRequest::Pingpong(greeting),
            address_manager: None,
        }
    }

    /// Get the protocol name for this request.
    fn protocol_name(&self) -> &'static str {
        match &self.request {
            TopologyOutboundRequest::Handshake => HANDSHAKE_PROTOCOL,
            TopologyOutboundRequest::Hive(_) => HIVE_PROTOCOL,
            TopologyOutboundRequest::Pingpong(_) => PINGPONG_PROTOCOL,
        }
    }
}

impl<N: SwarmNodeTypes> std::fmt::Debug for TopologyOutboundUpgrade<N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TopologyOutboundUpgrade")
            .field("peer_id", &self.peer_id)
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

impl<N: SwarmNodeTypes> UpgradeInfo for TopologyOutboundUpgrade<N> {
    type Info = &'static str;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(self.protocol_name())
    }
}

impl<N: SwarmNodeTypes> OutboundUpgrade<Stream> for TopologyOutboundUpgrade<N> {
    type Output = TopologyOutboundOutput;
    type Error = TopologyUpgradeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_outbound(self, socket: Stream, info: Self::Info) -> Self::Future {
        Box::pin(async move {
            match self.request {
                TopologyOutboundRequest::Handshake => {
                    // Get additional addresses from AddressManager if available
                    let additional_addrs = self
                        .address_manager
                        .as_ref()
                        .map(|mgr| mgr.addresses_for_peer(&self.remote_addr))
                        .unwrap_or_default();

                    debug!(
                        peer_id = %self.peer_id,
                        remote_addr = %self.remote_addr,
                        additional_addrs_count = additional_addrs.len(),
                        "Outbound handshake: selected addresses for peer"
                    );

                    let handshake = HandshakeProtocol::<N>::with_addrs(
                        self.identity,
                        self.peer_id,
                        self.remote_addr.clone(),
                        additional_addrs,
                    );
                    let result = handshake.upgrade_outbound(socket, info).await?;

                    // Report observed address to AddressManager if available
                    if let Some(mgr) = &self.address_manager {
                        debug!(
                            peer_id = %self.peer_id,
                            observed_addr = %result.observed_multiaddr(),
                            "Outbound handshake: reporting observed address"
                        );
                        mgr.on_observed_addr(
                            result.observed_multiaddr().clone(),
                            &self.remote_addr,
                        );
                    }

                    Ok(TopologyOutboundOutput::Handshake(Box::new(result)))
                }
                TopologyOutboundRequest::Hive(peers) => {
                    let hive: HiveOutboundProtocol = vertex_net_hive::outbound(&peers);
                    hive.upgrade_outbound(socket, info)
                        .await
                        .map_err(TopologyUpgradeError::Hive)?;
                    Ok(TopologyOutboundOutput::Hive)
                }
                TopologyOutboundRequest::Pingpong(greeting) => {
                    let pingpong: PingpongOutboundProtocol =
                        vertex_net_pingpong::outbound(greeting);
                    let pong = pingpong
                        .upgrade_outbound(socket, info)
                        .await
                        .map_err(TopologyUpgradeError::Pingpong)?;
                    Ok(TopologyOutboundOutput::Pingpong(pong))
                }
            }
        })
    }
}

// ============================================================================
// Info for Tracking Outbound Requests
// ============================================================================

/// Information about an outbound request, used for correlating responses.
#[derive(Debug, Clone)]
pub enum TopologyOutboundInfo {
    /// Handshake request.
    Handshake,
    /// Hive broadcast.
    Hive,
    /// Ping request with sent timestamp.
    Pingpong { sent_at: std::time::Instant },
}
