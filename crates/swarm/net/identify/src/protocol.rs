//! Wire protocol for identify messages.

use std::io;

use asynchronous_codec::{FramedRead, FramedWrite};
use futures::prelude::*;
use libp2p::core::{Multiaddr, PeerRecord, SignedEnvelope, multiaddr};
use libp2p::identity::{self as identity, PublicKey};
use libp2p::swarm::StreamProtocol;
use thiserror::Error;

use crate::generated as proto;

const MAX_MESSAGE_SIZE_BYTES: usize = 4096;

/// Identify information of a peer sent in protocol messages.
#[derive(Debug, Clone)]
pub struct Info {
    pub public_key: PublicKey,
    pub protocol_version: String,
    pub agent_version: String,
    pub listen_addrs: Vec<Multiaddr>,
    pub protocols: Vec<StreamProtocol>,
    pub observed_addr: Multiaddr,
    pub signed_peer_record: Option<SignedEnvelope>,
}

impl Info {
    pub fn merge(&mut self, info: PushInfo) {
        if let Some(public_key) = info.public_key {
            self.public_key = public_key;
        }
        if let Some(protocol_version) = info.protocol_version {
            self.protocol_version = protocol_version;
        }
        if let Some(agent_version) = info.agent_version {
            self.agent_version = agent_version;
        }
        if !info.listen_addrs.is_empty() {
            self.listen_addrs = info.listen_addrs;
        }
        if !info.protocols.is_empty() {
            self.protocols = info.protocols;
        }
        if let Some(observed_addr) = info.observed_addr {
            self.observed_addr = observed_addr;
        }
    }
}

/// Identify push information of a peer sent in protocol messages.
#[derive(Debug, Clone)]
pub struct PushInfo {
    pub public_key: Option<PublicKey>,
    pub protocol_version: Option<String>,
    pub agent_version: Option<String>,
    pub listen_addrs: Vec<Multiaddr>,
    pub protocols: Vec<StreamProtocol>,
    pub observed_addr: Option<Multiaddr>,
}

pub(crate) async fn send_identify<T>(io: T, info: Info) -> Result<Info, UpgradeError>
where
    T: AsyncWrite + Unpin,
{
    tracing::trace!("Sending: {:?}", info);

    let listen_addrs = info.listen_addrs.iter().map(|addr| addr.to_vec()).collect();

    let pubkey_bytes = info.public_key.encode_protobuf();

    let message = proto::Identify {
        agentVersion: Some(info.agent_version.clone()),
        protocolVersion: Some(info.protocol_version.clone()),
        publicKey: Some(pubkey_bytes),
        listenAddrs: listen_addrs,
        observedAddr: Some(info.observed_addr.to_vec()),
        protocols: info.protocols.iter().map(|p| p.to_string()).collect(),
        signedPeerRecord: info
            .signed_peer_record
            .clone()
            .map(|r| r.into_protobuf_encoding()),
    };

    let mut framed_io = FramedWrite::new(
        io,
        quick_protobuf_codec::Codec::<proto::Identify>::new(MAX_MESSAGE_SIZE_BYTES),
    );

    framed_io.send(message).await?;
    framed_io.close().await?;

    Ok(info)
}

pub(crate) async fn recv_push<T>(socket: T) -> Result<PushInfo, UpgradeError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let info = recv(socket).await?.try_into()?;
    tracing::trace!(?info, "Received");
    Ok(info)
}

pub(crate) async fn recv_identify<T>(socket: T) -> Result<Info, UpgradeError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let info = recv(socket).await?.try_into()?;
    tracing::trace!(?info, "Received");
    Ok(info)
}

async fn recv<T>(socket: T) -> Result<proto::Identify, UpgradeError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let info = FramedRead::new(
        socket,
        quick_protobuf_codec::Codec::<proto::Identify>::new(MAX_MESSAGE_SIZE_BYTES),
    )
    .next()
    .await
    .ok_or(UpgradeError::StreamClosed)??;

    Ok(info)
}

fn parse_listen_addrs(listen_addrs: Vec<Vec<u8>>) -> Vec<Multiaddr> {
    listen_addrs
        .into_iter()
        .filter_map(|bytes| match Multiaddr::try_from(bytes) {
            Ok(a) => Some(a),
            Err(e) => {
                tracing::debug!("Unable to parse multiaddr: {e:?}");
                None
            }
        })
        .collect()
}

fn parse_protocols(protocols: Vec<String>) -> Vec<StreamProtocol> {
    protocols
        .into_iter()
        .filter_map(|p| match StreamProtocol::try_from_owned(p) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::debug!("Received invalid protocol from peer: {e}");
                None
            }
        })
        .collect()
}

fn parse_public_key(public_key: Option<Vec<u8>>) -> Option<PublicKey> {
    public_key.and_then(|key| match PublicKey::try_decode_protobuf(&key) {
        Ok(k) => Some(k),
        Err(e) => {
            tracing::debug!("Unable to decode public key: {e:?}");
            None
        }
    })
}

fn parse_observed_addr(observed_addr: Option<Vec<u8>>) -> Option<Multiaddr> {
    observed_addr.and_then(|bytes| match Multiaddr::try_from(bytes) {
        Ok(a) => Some(a),
        Err(e) => {
            tracing::debug!("Unable to parse observed multiaddr: {e:?}");
            None
        }
    })
}

impl TryFrom<proto::Identify> for Info {
    type Error = UpgradeError;

    fn try_from(msg: proto::Identify) -> Result<Self, Self::Error> {
        let identify_public_key = {
            match parse_public_key(msg.publicKey) {
                Some(key) => key,
                None => PublicKey::try_decode_protobuf(Default::default())?,
            }
        };

        let (listen_addrs, signed_envelope) = msg
            .signedPeerRecord
            .and_then(|b| {
                let envelope = SignedEnvelope::from_protobuf_encoding(b.as_ref()).ok()?;
                let peer_record = PeerRecord::from_signed_envelope(envelope).ok()?;
                (peer_record.peer_id() == identify_public_key.to_peer_id()).then_some((
                    peer_record.addresses().to_vec(),
                    Some(peer_record.into_signed_envelope()),
                ))
            })
            .unwrap_or_else(|| (parse_listen_addrs(msg.listenAddrs), None));

        let info = Info {
            public_key: identify_public_key,
            protocol_version: msg.protocolVersion.unwrap_or_default(),
            agent_version: msg.agentVersion.unwrap_or_default(),
            listen_addrs,
            protocols: parse_protocols(msg.protocols),
            observed_addr: parse_observed_addr(msg.observedAddr).unwrap_or(Multiaddr::empty()),
            signed_peer_record: signed_envelope,
        };

        Ok(info)
    }
}

impl TryFrom<proto::Identify> for PushInfo {
    type Error = UpgradeError;

    fn try_from(msg: proto::Identify) -> Result<Self, Self::Error> {
        let info = PushInfo {
            public_key: parse_public_key(msg.publicKey),
            protocol_version: msg.protocolVersion,
            agent_version: msg.agentVersion,
            listen_addrs: parse_listen_addrs(msg.listenAddrs),
            protocols: parse_protocols(msg.protocols),
            observed_addr: parse_observed_addr(msg.observedAddr),
        };

        Ok(info)
    }
}

#[derive(Debug, Error)]
pub enum UpgradeError {
    #[error(transparent)]
    Codec(#[from] quick_protobuf_codec::Error),
    #[error("I/O interaction failed")]
    Io(#[from] io::Error),
    #[error("Stream closed")]
    StreamClosed,
    #[error("Failed decoding multiaddr")]
    Multiaddr(#[from] multiaddr::Error),
    #[error("Failed decoding public key")]
    PublicKey(#[from] identity::DecodingError),
}
