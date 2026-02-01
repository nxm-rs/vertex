use asynchronous_codec::{Framed, FramedRead};
use futures::{AsyncWriteExt, SinkExt, TryStreamExt, future::BoxFuture};
use libp2p::{InboundUpgrade, Multiaddr, OutboundUpgrade, PeerId, Stream, core::UpgradeInfo};
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_api::SwarmNodeTypes;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarmspec::SwarmSpec;

use crate::{
    Ack, AckCodec, HandshakeError, HandshakeInfo, PROTOCOL, Syn, SynAck, SynAckCodec, SynCodec,
    codec::HandshakeCodecDomainError,
};

/// Handshake protocol upgrade for Swarm peer authentication.
///
/// This protocol handles the three-way handshake (SYN → SYNACK → ACK)
/// that authenticates peers on the Swarm network.
///
/// Generic over `N: SwarmNodeTypes` to support different node configurations.
#[derive(Clone)]
pub struct HandshakeProtocol<N: SwarmNodeTypes> {
    pub(crate) identity: N::Identity,
    pub(crate) peer_id: PeerId,
    pub(crate) remote_addr: Multiaddr,
    /// Additional addresses to advertise (in addition to observed address).
    ///
    /// These are typically provided by an AddressManager based on the peer's
    /// network scope (public, private, etc.).
    pub(crate) additional_addrs: Vec<Multiaddr>,
}

impl<N: SwarmNodeTypes> HandshakeProtocol<N> {
    /// Create a new handshake protocol.
    pub fn new(identity: N::Identity, peer_id: PeerId, remote_addr: Multiaddr) -> Self {
        Self {
            identity,
            peer_id,
            remote_addr,
            additional_addrs: Vec::new(),
        }
    }

    /// Create a handshake protocol with additional addresses to advertise.
    ///
    /// The `additional_addrs` are combined with the observed address when
    /// creating our identity for the handshake. Use this to advertise
    /// NAT addresses, listen addresses, etc.
    pub fn with_addrs(
        identity: N::Identity,
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
    ///
    /// Flow: Receive SYN → Send SYNACK → Receive ACK
    async fn handle_inbound(self, stream: Stream) -> Result<HandshakeInfo, HandshakeError> {
        let network_id = self.identity.spec().network_id();

        // Set up codecs (all use network_id as context for validation/encoding)
        let syn_codec = SynCodec::new(1024);
        let synack_codec = SynAckCodec::new(1024, network_id);
        let ack_codec = AckCodec::new(1024, network_id);

        // Read SYN using framed read
        let mut framed = FramedRead::new(stream, syn_codec);
        let syn = framed
            .try_next()
            .await?
            .ok_or(HandshakeError::ConnectionClosed)?;

        // Get the address the peer observes us at (for NAT discovery)
        let observed_multiaddr = syn.observed_multiaddr().clone();

        // Combine observed address with additional addresses for our identity
        let mut our_addrs = vec![observed_multiaddr.clone()];
        our_addrs.extend(
            self.additional_addrs
                .iter()
                .filter(|a| *a != &observed_multiaddr)
                .cloned(),
        );

        // Create local SwarmPeer identity with all addresses
        let local_peer = SwarmPeer::from_identity(&self.identity, our_addrs).map_err(|e| {
            HandshakeError::Codec(crate::codec::CodecError::domain(
                HandshakeCodecDomainError::InvalidPeer(e),
            ))
        })?;

        // Send SYNACK
        let synack = SynAck::new(
            syn,
            local_peer,
            self.identity.is_full_node(),
            self.identity
                .welcome_message()
                .unwrap_or_default()
                .to_string(),
        );

        let mut framed = Framed::new(framed.into_inner(), synack_codec);
        framed.send(synack).await?;

        // Read ACK (network_id validated by codec)
        let mut framed = FramedRead::new(framed.into_inner(), ack_codec);
        let ack = framed
            .try_next()
            .await?
            .ok_or(HandshakeError::ConnectionClosed)?;

        framed.close().await?;

        // Extract components from received Ack
        let (swarm_peer, full_node, welcome_message) = ack.into_parts();

        Ok(HandshakeInfo {
            peer_id: self.peer_id,
            swarm_peer,
            full_node,
            welcome_message,
            observed_multiaddr,
        })
    }

    /// Handle an outbound handshake (we are the dialer).
    ///
    /// Flow: Send SYN → Receive SYNACK → Send ACK
    async fn handle_outbound(self, stream: Stream) -> Result<HandshakeInfo, HandshakeError> {
        let network_id = self.identity.spec().network_id();

        // Construct the observed multiaddr with the peer ID appended
        // Format: /ip4/x.x.x.x/tcp/1634/p2p/QmPeerId...
        // This is what we tell the remote peer we see them at
        let their_observed_multiaddr = self
            .remote_addr
            .clone()
            .with(libp2p::multiaddr::Protocol::P2p(self.peer_id));

        // Set up codecs (all use network_id as context for validation/encoding)
        let syn_codec = SynCodec::new(1024);
        let synack_codec = SynAckCodec::new(1024, network_id);
        let ack_codec = AckCodec::new(1024, network_id);

        let mut framed = Framed::new(stream, syn_codec);
        framed.send(Syn::new(their_observed_multiaddr)).await?;

        // Read SYNACK (network_id validated by codec)
        let mut framed = FramedRead::new(framed.into_inner(), synack_codec);
        let syn_ack = framed
            .try_next()
            .await?
            .ok_or(HandshakeError::ConnectionClosed)?;

        // Get the address the peer observes us at (for NAT discovery)
        // This comes from the echoed SYN in the SYNACK
        let observed_multiaddr = syn_ack.syn().observed_multiaddr().clone();

        // Combine observed address with additional addresses for our identity
        let mut our_addrs = vec![observed_multiaddr.clone()];
        our_addrs.extend(
            self.additional_addrs
                .iter()
                .filter(|a| *a != &observed_multiaddr)
                .cloned(),
        );

        // Create local SwarmPeer identity with all addresses
        let local_peer = SwarmPeer::from_identity(&self.identity, our_addrs).map_err(|e| {
            HandshakeError::Codec(crate::codec::CodecError::domain(
                HandshakeCodecDomainError::InvalidPeer(e),
            ))
        })?;

        // Send ACK (network_id provided by codec context)
        let ack = Ack::new(
            local_peer,
            self.identity.is_full_node(),
            self.identity
                .welcome_message()
                .unwrap_or_default()
                .to_string(),
        );

        let mut framed = Framed::new(framed.into_inner(), ack_codec);
        framed.send(ack).await?;
        framed.close().await?;

        // Extract components from received SynAck
        let (_, swarm_peer, full_node, welcome_message) = syn_ack.into_parts();

        Ok(HandshakeInfo {
            peer_id: self.peer_id,
            swarm_peer,
            full_node,
            welcome_message,
            observed_multiaddr,
        })
    }
}

impl<N: SwarmNodeTypes> UpgradeInfo for HandshakeProtocol<N> {
    type Info = &'static str;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(PROTOCOL)
    }
}

impl<N: SwarmNodeTypes> InboundUpgrade<Stream> for HandshakeProtocol<N> {
    type Output = HandshakeInfo;
    type Error = HandshakeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, socket: Stream, _: Self::Info) -> Self::Future {
        Box::pin(self.handle_inbound(socket))
    }
}

impl<N: SwarmNodeTypes> OutboundUpgrade<Stream> for HandshakeProtocol<N> {
    type Output = HandshakeInfo;
    type Error = HandshakeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_outbound(self, socket: Stream, _: Self::Info) -> Self::Future {
        Box::pin(self.handle_outbound(socket))
    }
}
