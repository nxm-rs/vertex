use std::sync::Arc;

use asynchronous_codec::{Framed, FramedRead};
use futures::{AsyncWriteExt, SinkExt, TryStreamExt, future::BoxFuture};
use libp2p::{InboundUpgrade, Multiaddr, OutboundUpgrade, PeerId, Stream, core::UpgradeInfo};
use tracing::{debug, info};
use vertex_net_primitives::NodeAddress;

use crate::{
    Ack, AckCodec, HandshakeConfig, HandshakeError, HandshakeInfo, PROTOCOL, Syn, SynAck,
    SynAckCodec, SynCodec,
};

pub struct HandshakeProtocol<C: HandshakeConfig> {
    pub(crate) config: Arc<C>,
    pub(crate) peer_id: PeerId,
    pub(crate) remote_addr: Multiaddr,
}

impl<C: HandshakeConfig> HandshakeProtocol<C> {
    /// Create a new handshake protocol.
    pub fn new(config: Arc<C>, peer_id: PeerId, remote_addr: Multiaddr) -> Self {
        Self {
            config,
            peer_id,
            remote_addr,
        }
    }
}

impl<C: HandshakeConfig> Clone for HandshakeProtocol<C> {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            peer_id: self.peer_id,
            remote_addr: self.remote_addr.clone(),
        }
    }
}

impl<C: HandshakeConfig> std::fmt::Debug for HandshakeProtocol<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HandshakeProtocol")
            .field("peer_id", &self.peer_id)
            .field("remote_addr", &self.remote_addr)
            .finish_non_exhaustive()
    }
}

impl<C: HandshakeConfig> UpgradeInfo for HandshakeProtocol<C> {
    type Info = &'static str;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(PROTOCOL)
    }
}

impl<C: HandshakeConfig> InboundUpgrade<Stream> for HandshakeProtocol<C> {
    type Output = HandshakeInfo;
    type Error = HandshakeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, socket: Stream, _: Self::Info) -> Self::Future {
        Box::pin(handle_inbound_handshake(
            socket,
            self.config,
            self.peer_id,
            self.remote_addr,
        ))
    }
}

impl<C: HandshakeConfig> OutboundUpgrade<Stream> for HandshakeProtocol<C> {
    type Output = HandshakeInfo;
    type Error = HandshakeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_outbound(self, socket: Stream, _: Self::Info) -> Self::Future {
        Box::pin(handle_outbound_handshake(
            socket,
            self.config,
            self.peer_id,
            self.remote_addr,
        ))
    }
}

async fn handle_inbound_handshake<C: HandshakeConfig>(
    stream: Stream,
    config: Arc<C>,
    peer_id: PeerId,
    _: Multiaddr,
) -> Result<HandshakeInfo, HandshakeError> {
    let network_id = config.network_id();

    // Set up codecs - SynAckCodec and AckCodec need network_id for validation
    let syn_codec = SynCodec::new(1024);
    let synack_codec = SynAckCodec::new(1024, network_id);
    let ack_codec = AckCodec::new(1024, network_id);

    // Read SYN using framed read
    let mut framed = FramedRead::new(stream, syn_codec);
    debug!("Attempting to read SYN");
    let syn = framed
        .try_next()
        .await?
        .ok_or(HandshakeError::ConnectionClosed)?;
    debug!("Received SYN: {:?}", syn);

    // Create local address
    let local_address = NodeAddress::builder()
        .with_network_id(network_id)
        .with_nonce(config.nonce())
        .with_underlay(syn.observed_underlay().clone())
        .with_signer(config.signer())
        .map_err(|e| HandshakeError::Codec(e.into()))?
        .build();

    // Send SYNACK
    let synack = SynAck::new(
        syn,
        Ack::new(
            local_address,
            config.is_full_node(),
            config.welcome_message().unwrap_or_default(),
        )?,
    );

    let mut framed = Framed::new(framed.into_inner(), synack_codec);
    framed.send(synack).await?;

    // Read ACK
    let mut framed = FramedRead::new(framed.into_inner(), ack_codec);
    debug!("Attempting to read ACK");
    let ack = framed
        .try_next()
        .await?
        .ok_or(HandshakeError::ConnectionClosed)?;
    debug!("Received ACK: {:?}", ack);

    framed.close().await?;

    Ok(HandshakeInfo { peer_id, ack })
}

async fn handle_outbound_handshake<C: HandshakeConfig>(
    stream: Stream,
    config: Arc<C>,
    peer_id: PeerId,
    remote_addr: Multiaddr,
) -> Result<HandshakeInfo, HandshakeError> {
    let network_id = config.network_id();

    // Construct the observed underlay with the peer ID appended
    // Format: /ip4/x.x.x.x/tcp/1634/p2p/QmPeerId...
    let observed_underlay = remote_addr.with(libp2p::multiaddr::Protocol::P2p(peer_id));
    info!("Observed underlay: {:?}", observed_underlay);

    // Set up codecs - SynAckCodec and AckCodec need network_id for validation
    let syn_codec = SynCodec::new(1024);
    let synack_codec = SynAckCodec::new(1024, network_id);
    let ack_codec = AckCodec::new(1024, network_id);

    let mut framed = Framed::new(stream, syn_codec);
    framed.send(Syn::new(observed_underlay)).await?;

    // Read SYNACK
    let mut framed = FramedRead::new(framed.into_inner(), synack_codec);
    debug!("Attempting to read SYNACK");
    let syn_ack = framed
        .try_next()
        .await?
        .ok_or(HandshakeError::ConnectionClosed)?;
    debug!("Received SYNACK: {:?}", syn_ack);

    let local_address = NodeAddress::builder()
        .with_network_id(network_id)
        .with_nonce(config.nonce())
        .with_underlay(syn_ack.syn().observed_underlay().clone())
        .with_signer(config.signer())
        .map_err(|e| HandshakeError::Codec(e.into()))?
        .build();

    // Send ACK
    let mut framed = Framed::new(framed.into_inner(), ack_codec);

    framed
        .send(Ack::new(
            local_address,
            config.is_full_node(),
            config.welcome_message().unwrap_or_default(),
        )?)
        .await?;

    framed.close().await?;

    // Create HandshakeInfo from received data
    let (_, ack) = syn_ack.into_parts();
    Ok(HandshakeInfo { peer_id, ack })
}
