use std::sync::Arc;

use asynchronous_codec::{Framed, FramedRead};
use futures::{future::BoxFuture, AsyncWriteExt, SinkExt, TryStreamExt};
use libp2p::{core::UpgradeInfo, InboundUpgrade, Multiaddr, OutboundUpgrade, PeerId, Stream};
use tracing::{debug, info};
use vertex_network_primitives::NodeAddress;
use vertex_node_core::args::NodeCommand;

use crate::{
    Ack, AckCodec, HandshakeError, HandshakeInfo, Syn, SynAck, SynAckCodec, SynCodec, PROTOCOL,
};

#[derive(Debug, Clone)]
pub struct HandshakeProtocol<const N: u64> {
    pub(crate) config: Arc<NodeCommand>,
    pub(crate) peer_id: PeerId,
    pub(crate) remote_addr: Multiaddr,
}

impl<const N: u64> UpgradeInfo for HandshakeProtocol<N> {
    type Info = &'static str;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(PROTOCOL)
    }
}

impl<const N: u64> InboundUpgrade<Stream> for HandshakeProtocol<N> {
    type Output = HandshakeInfo<N>;
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

impl<const N: u64> OutboundUpgrade<Stream> for HandshakeProtocol<N> {
    type Output = HandshakeInfo<N>;
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

async fn handle_inbound_handshake<const N: u64>(
    stream: Stream,
    config: Arc<NodeCommand>,
    peer_id: PeerId,
    _: Multiaddr,
) -> Result<HandshakeInfo<N>, HandshakeError> {
    // Set up codecs
    let syn_codec = SynCodec::<N>::new(1024);
    let synack_codec = SynAckCodec::<N>::new(1024);
    let ack_codec = AckCodec::<N>::new(1024);

    // Read SYN using framed read
    let mut framed = FramedRead::new(stream, syn_codec);
    debug!("Attempting to read SYN");
    let syn = framed
        .try_next()
        .await?
        .ok_or(HandshakeError::ConnectionClosed)?;
    debug!("Received SYN: {:?}", syn);

    // Create local address
    let local_address: NodeAddress<N> = NodeAddress::builder()
        .with_nonce(config.neighbourhood.nonce)
        .with_underlay(syn.observed_underlay().clone())
        .with_signer(config.wallet.signer())
        .map_err(|e| HandshakeError::Codec(e.into()))?
        .build();

    // Send SYNACK
    let synack = SynAck::new(
        syn,
        Ack::new(
            local_address,
            config.neighbourhood.full,
            config.network.welcome_message.clone(),
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

async fn handle_outbound_handshake<const N: u64>(
    stream: Stream,
    config: Arc<NodeCommand>,
    peer_id: PeerId,
    remote_addr: Multiaddr,
) -> Result<HandshakeInfo<N>, HandshakeError> {
    info!("Remote address: {:?}", remote_addr);

    let syn_codec = SynCodec::<N>::new(1024);
    let synack_codec = SynAckCodec::<N>::new(1024);
    let ack_codec = AckCodec::<N>::new(1024);

    let mut framed = Framed::new(stream, syn_codec);
    framed.send(Syn::new(remote_addr)).await?;

    // Read SYNACK
    let mut framed = FramedRead::new(framed.into_inner(), synack_codec);
    debug!("Attempting to read SYNACK");
    let syn_ack = framed
        .try_next()
        .await?
        .ok_or(HandshakeError::ConnectionClosed)?;
    debug!("Received SYNACK: {:?}", syn_ack);

    let local_address: NodeAddress<N> = NodeAddress::builder()
        .with_nonce(config.neighbourhood.nonce)
        .with_underlay(syn_ack.syn().observed_underlay().clone())
        .with_signer(config.wallet.signer())
        .map_err(|e| HandshakeError::Codec(e.into()))?
        .build();

    // Send ACK
    let mut framed = Framed::new(framed.into_inner(), ack_codec);

    framed
        .send(Ack::new(
            local_address,
            config.neighbourhood.full,
            config.network.welcome_message.clone(),
        )?)
        .await?;

    framed.close().await?;

    // Create HandshakeInfo from received data
    let (_, ack) = syn_ack.into_parts();
    Ok(HandshakeInfo { peer_id, ack })
}
