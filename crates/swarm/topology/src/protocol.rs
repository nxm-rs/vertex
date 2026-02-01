//! Protocol upgrades for topology handler.
//!
//! Provides multi-protocol support combining handshake, hive, and pingpong into
//! unified inbound/outbound upgrades for the connection handler.

use std::sync::Arc;

use futures::future::BoxFuture;
use libp2p::{InboundUpgrade, Multiaddr, OutboundUpgrade, PeerId, Stream, core::UpgradeInfo};
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

/// Errors from topology protocol upgrades.
#[derive(Debug, thiserror::Error)]
pub enum TopologyUpgradeError {
    #[error("handshake error: {0}")]
    Handshake(#[from] HandshakeError),

    #[error("hive error: {0}")]
    Hive(#[source] ProtocolError),

    #[error("pingpong error: {0}")]
    Pingpong(#[source] ProtocolError),

    #[error("unknown protocol: {0}")]
    UnknownProtocol(String),
}

/// Output from an inbound topology upgrade.
pub enum TopologyInboundOutput {
    Handshake(Box<HandshakeInfo>),
    Hive(Vec<SwarmPeer>),
    Pingpong,
}

/// Inbound upgrade that handles handshake, hive, and pingpong protocols.
#[derive(Clone)]
pub struct TopologyInboundUpgrade<N: SwarmNodeTypes> {
    identity: N::Identity,
    peer_id: PeerId,
    remote_addr: Multiaddr,
    address_manager: Option<Arc<AddressManager>>,
}

impl<N: SwarmNodeTypes> TopologyInboundUpgrade<N> {
    pub fn new(identity: N::Identity, peer_id: PeerId, remote_addr: Multiaddr) -> Self {
        Self {
            identity,
            peer_id,
            remote_addr,
            address_manager: None,
        }
    }

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

/// Type of outbound request.
#[derive(Debug, Clone)]
pub enum TopologyOutboundRequest {
    Handshake,
    Hive(Vec<SwarmPeer>),
    Pingpong(String),
}

/// Output from an outbound topology upgrade.
pub enum TopologyOutboundOutput {
    Handshake(Box<HandshakeInfo>),
    Hive,
    Pingpong(Pong),
}

/// Outbound upgrade for a specific topology protocol.
#[derive(Clone)]
pub struct TopologyOutboundUpgrade<N: SwarmNodeTypes> {
    identity: N::Identity,
    peer_id: PeerId,
    remote_addr: Multiaddr,
    request: TopologyOutboundRequest,
    address_manager: Option<Arc<AddressManager>>,
}

impl<N: SwarmNodeTypes> TopologyOutboundUpgrade<N> {
    pub fn handshake(identity: N::Identity, peer_id: PeerId, remote_addr: Multiaddr) -> Self {
        Self {
            identity,
            peer_id,
            remote_addr,
            request: TopologyOutboundRequest::Handshake,
            address_manager: None,
        }
    }

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

/// Info for tracking outbound requests.
#[derive(Debug, Clone)]
pub enum TopologyOutboundInfo {
    Handshake,
    Hive,
    Pingpong { sent_at: std::time::Instant },
}
