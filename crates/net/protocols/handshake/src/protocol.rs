use asynchronous_codec::{Framed, FramedRead};
use futures::{AsyncWriteExt, SinkExt, TryStreamExt, future::BoxFuture};
use libp2p::{InboundUpgrade, Multiaddr, OutboundUpgrade, PeerId, Stream, core::UpgradeInfo};
use tracing::instrument;
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_spec::SwarmSpec;

use crate::{
    Ack, AckCodec, HandshakeError, HandshakeInfo, PROTOCOL, Syn, SynAck, SynAckCodec, SynCodec,
    metrics::HandshakeMetrics,
};

/// Handshake protocol upgrade for Swarm peer authentication.
pub struct HandshakeProtocol<I: SwarmIdentity> {
    identity: I,
    peer_id: PeerId,
    remote_addr: Multiaddr,
    additional_addrs: Vec<Multiaddr>,
}

impl<I: SwarmIdentity> HandshakeProtocol<I> {
    /// Create a new handshake protocol.
    pub fn new(identity: I, peer_id: PeerId, remote_addr: Multiaddr) -> Self {
        Self {
            identity,
            peer_id,
            remote_addr,
            additional_addrs: Vec::new(),
        }
    }

    /// Create a handshake protocol with additional addresses to advertise.
    pub fn with_addrs(
        identity: I,
        peer_id: PeerId,
        remote_addr: Multiaddr,
        additional_addrs: Vec<Multiaddr>,
    ) -> Self {
        Self {
            identity,
            peer_id,
            remote_addr,
            additional_addrs,
        }
    }

    /// Handle an inbound handshake (we are the listener).
    #[instrument(
        name = "handshake_inbound",
        skip(self, stream),
        fields(
            peer_id = %self.peer_id,
            remote_addr = %self.remote_addr,
        )
    )]
    async fn handle_inbound(self, stream: Stream) -> Result<HandshakeInfo, HandshakeError> {
        let metrics = HandshakeMetrics::inbound();

        match self.handle_inbound_inner(stream).await {
            Ok(info) => {
                metrics.record_success(info.node_type);
                Ok(info)
            }
            Err(e) => {
                metrics.record_failure(&e);
                Err(e)
            }
        }
    }

    async fn handle_inbound_inner(self, stream: Stream) -> Result<HandshakeInfo, HandshakeError> {
        let network_id = self.identity.spec().network_id();

        let syn_codec = SynCodec::new(1024);
        let synack_codec = SynAckCodec::new(1024, network_id);
        let ack_codec = AckCodec::new(1024, network_id);

        // Read SYN
        let mut framed = FramedRead::new(stream, syn_codec);
        let syn = framed
            .try_next()
            .await?
            .ok_or(HandshakeError::ConnectionClosed)?;

        let observed_multiaddr = syn.observed_multiaddr().clone();

        // Combine addresses for our identity
        let mut our_addrs = vec![observed_multiaddr.clone()];
        our_addrs.extend(
            self.additional_addrs
                .iter()
                .filter(|a| *a != &observed_multiaddr)
                .cloned(),
        );

        let local_peer = SwarmPeer::from_identity(&self.identity, our_addrs)?;

        // Send SYNACK
        let synack = SynAck::new(
            syn,
            local_peer,
            self.identity.node_type(),
            self.identity
                .welcome_message()
                .unwrap_or_default()
                .to_string(),
        );

        let mut framed = Framed::new(framed.into_inner(), synack_codec);
        framed.send(synack).await?;

        // Read ACK
        let mut framed = FramedRead::new(framed.into_inner(), ack_codec);
        let ack = framed
            .try_next()
            .await?
            .ok_or(HandshakeError::ConnectionClosed)?;

        framed.close().await?;

        let (swarm_peer, node_type, welcome_message) = ack.into_parts();

        Ok(HandshakeInfo {
            peer_id: self.peer_id,
            swarm_peer,
            node_type,
            welcome_message,
            observed_multiaddr,
        })
    }

    /// Handle an outbound handshake (we are the dialer).
    #[instrument(
        name = "handshake_outbound",
        skip(self, stream),
        fields(
            peer_id = %self.peer_id,
            remote_addr = %self.remote_addr,
        )
    )]
    async fn handle_outbound(self, stream: Stream) -> Result<HandshakeInfo, HandshakeError> {
        let metrics = HandshakeMetrics::outbound();

        match self.handle_outbound_inner(stream).await {
            Ok(info) => {
                metrics.record_success(info.node_type);
                Ok(info)
            }
            Err(e) => {
                metrics.record_failure(&e);
                Err(e)
            }
        }
    }

    async fn handle_outbound_inner(self, stream: Stream) -> Result<HandshakeInfo, HandshakeError> {
        let network_id = self.identity.spec().network_id();

        let their_observed_multiaddr = self
            .remote_addr
            .clone()
            .with(libp2p::multiaddr::Protocol::P2p(self.peer_id));

        let syn_codec = SynCodec::new(1024);
        let synack_codec = SynAckCodec::new(1024, network_id);
        let ack_codec = AckCodec::new(1024, network_id);

        // Send SYN
        let mut framed = Framed::new(stream, syn_codec);
        framed.send(Syn::new(their_observed_multiaddr)).await?;

        // Read SYNACK
        let mut framed = FramedRead::new(framed.into_inner(), synack_codec);
        let syn_ack = framed
            .try_next()
            .await?
            .ok_or(HandshakeError::ConnectionClosed)?;

        let observed_multiaddr = syn_ack.syn().observed_multiaddr().clone();

        // Combine addresses for our identity
        let mut our_addrs = vec![observed_multiaddr.clone()];
        our_addrs.extend(
            self.additional_addrs
                .iter()
                .filter(|a| *a != &observed_multiaddr)
                .cloned(),
        );

        let local_peer = SwarmPeer::from_identity(&self.identity, our_addrs)?;

        // Send ACK
        let ack = Ack::new(
            local_peer,
            self.identity.node_type(),
            self.identity
                .welcome_message()
                .unwrap_or_default()
                .to_string(),
        );

        let mut framed = Framed::new(framed.into_inner(), ack_codec);
        framed.send(ack).await?;
        framed.close().await?;

        let (_, swarm_peer, node_type, welcome_message) = syn_ack.into_parts();

        Ok(HandshakeInfo {
            peer_id: self.peer_id,
            swarm_peer,
            node_type,
            welcome_message,
            observed_multiaddr,
        })
    }
}

impl<I: SwarmIdentity> UpgradeInfo for HandshakeProtocol<I> {
    type Info = &'static str;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(PROTOCOL)
    }
}

impl<I: SwarmIdentity> InboundUpgrade<Stream> for HandshakeProtocol<I> {
    type Output = HandshakeInfo;
    type Error = HandshakeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, socket: Stream, _: Self::Info) -> Self::Future {
        Box::pin(self.handle_inbound(socket))
    }
}

impl<I: SwarmIdentity> OutboundUpgrade<Stream> for HandshakeProtocol<I> {
    type Output = HandshakeInfo;
    type Error = HandshakeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_outbound(self, socket: Stream, _: Self::Info) -> Self::Future {
        Box::pin(self.handle_outbound(socket))
    }
}
