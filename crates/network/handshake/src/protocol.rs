use std::sync::Arc;

use asynchronous_codec::{Framed, FramedRead};
use futures::{future::BoxFuture, AsyncWriteExt, SinkExt, StreamExt};
use libp2p::{core::UpgradeInfo, InboundUpgrade, Multiaddr, OutboundUpgrade, PeerId, Stream};
use tracing::{debug, info};
use vertex_network_primitives::NodeAddress;

use crate::{
    Ack, AckCodec, HandshakeConfig, HandshakeError, HandshakeInfo, Syn, SynAck, SynAckCodec,
    SynCodec, PROTOCOL,
};

#[derive(Debug, Clone)]
pub struct HandshakeProtocol<const N: u64> {
    pub(crate) config: Arc<HandshakeConfig<N>>,
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
    config: Arc<HandshakeConfig<N>>,
    peer_id: PeerId,
    remote_addr: Multiaddr,
) -> Result<HandshakeInfo<N>, HandshakeError> {
    // Set up codecs
    let syn_codec = SynCodec::<N>::new(1024);
    let synack_codec = SynAckCodec::<N>::new(1024);
    let ack_codec = AckCodec::<N>::new(1024);

    // Read SYN using framed read
    let mut framed = FramedRead::new(stream, syn_codec);
    debug!("Attempting to read SYN");
    let syn = match framed.next().await {
        Some(Ok(syn)) => syn,
        Some(Err(e)) => return Err(e.into()),
        None => {
            debug!("Connection closed while reading SYN");
            return Err(HandshakeError::Stream(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Connection closed",
            )));
        }
    };
    debug!("Received SYN: {:?}", syn);

    // Create local address
    let local_address: NodeAddress<N> = NodeAddress::builder()
        .with_nonce(config.nonce)
        .with_underlay(syn.observed_underlay.clone())
        .with_signer(config.wallet.clone())
        .map_err(|e| HandshakeError::Codec(e.into()))?
        .build();

    // Send SYNACK
    let synack = SynAck {
        syn,
        ack: Ack {
            node_address: local_address,
            full_node: config.full_node,
            welcome_message: config.welcome_message.clone(),
        },
    };

    let mut framed = Framed::new(framed.into_inner(), synack_codec);
    framed.send(synack).await?;

    // Read ACK
    let mut framed = FramedRead::new(framed.into_inner(), ack_codec);
    debug!("Attempting to read ACK");
    let ack = match framed.next().await {
        Some(Ok(ack)) => ack,
        Some(Err(e)) => return Err(e.into()),
        None => {
            debug!("Connection closed before ACK was received");
            return Err(HandshakeError::Stream(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Connection closed",
            )));
        }
    };
    debug!("Received ACK: {:?}", ack);

    framed.close().await?;

    Ok(HandshakeInfo {
        peer_id,
        address: ack.node_address,
        full_node: ack.full_node,
        welcome_message: ack.welcome_message,
    })
}

async fn handle_outbound_handshake<const N: u64>(
    stream: Stream,
    config: Arc<HandshakeConfig<N>>,
    peer_id: PeerId,
    remote_addr: Multiaddr,
) -> Result<HandshakeInfo<N>, HandshakeError> {
    info!("Remote address: {:?}", remote_addr);

    let syn_codec = SynCodec::<N>::new(1024);
    let synack_codec = SynAckCodec::<N>::new(1024);
    let ack_codec = AckCodec::<N>::new(1024);

    let mut framed = Framed::new(stream, syn_codec);
    framed
        .send(
            Syn {
                observed_underlay: remote_addr.clone(),
            }
            .into(),
        )
        .await?;

    // Read SYNACK
    let mut framed = FramedRead::new(framed.into_inner(), synack_codec);
    debug!("Attempting to read SYNACK");
    let syn_ack = match framed.next().await {
        Some(Ok(syn)) => syn,
        Some(Err(e)) => return Err(e.into()),
        None => {
            debug!("Connection closed before SYNACK was received");
            return Err(HandshakeError::Stream(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Connection closed",
            )));
        }
    };
    debug!("Received SYNACK: {:?}", syn_ack);

    let local_address: NodeAddress<N> = NodeAddress::builder()
        .with_nonce(config.nonce)
        .with_underlay(syn_ack.syn.observed_underlay.clone())
        .with_signer(config.wallet.clone())
        .map_err(|e| HandshakeError::Codec(e.into()))?
        .build();

    // Send ACK
    let mut framed = Framed::new(framed.into_inner(), ack_codec);

    framed
        .send(Ack {
            node_address: local_address,
            full_node: config.full_node,
            welcome_message: config.welcome_message.clone(),
        })
        .await?;

    framed.close().await?;

    // Create HandshakeInfo from received data
    Ok(HandshakeInfo {
        peer_id,
        address: syn_ack.ack.node_address,
        full_node: syn_ack.ack.full_node,
        welcome_message: syn_ack.ack.welcome_message,
    })
}
