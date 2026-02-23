use libp2p::multiaddr::Protocol;
use libp2p::{Multiaddr, PeerId, Stream};
use tracing::{Instrument, Span, debug_span, instrument, warn};
use vertex_net_codec::FramedProto;
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_spec::SwarmSpec;

use crate::codec::{decode_ack, decode_syn, decode_synack, encode_ack, encode_syn, encode_synack};
use crate::metrics::HandshakeMetrics;
use crate::{HandshakeError, HandshakeInfo};

/// Maximum size for handshake message buffers.
const MAX_HANDSHAKE_BUFFER_SIZE: usize = 1024;

type Framed = FramedProto<MAX_HANDSHAKE_BUFFER_SIZE>;

/// Validate that observed address contains the expected peer ID.
fn validate_observed_addr(
    observed: &Multiaddr,
    expected_peer_id: &PeerId,
) -> Result<(), HandshakeError> {
    let peer_id_from_addr = observed
        .iter()
        .find_map(|p| if let Protocol::P2p(pid) = p { Some(pid) } else { None });

    match peer_id_from_addr {
        Some(pid) if &pid == expected_peer_id => Ok(()),
        Some(pid) => {
            warn!(%pid, expected = %expected_peer_id, "observed address has wrong peer ID");
            Err(HandshakeError::InvalidObservedAddress)
        }
        None => {
            warn!(%observed, "observed address missing peer ID");
            Err(HandshakeError::InvalidObservedAddress)
        }
    }
}

/// Combine additional addresses with the observed address and create a signed SwarmPeer.
///
/// Additional addresses come first, observed address is appended last (most important).
/// Duplicates of the observed address are filtered from additional addresses.
fn prepare_local_peer<I: SwarmIdentity>(
    identity: &I,
    additional_addrs: &[Multiaddr],
    observed_addr: &Multiaddr,
) -> Result<SwarmPeer, HandshakeError> {
    let mut addrs: Vec<Multiaddr> = additional_addrs
        .iter()
        .filter(|a| *a != observed_addr)
        .cloned()
        .collect();
    addrs.push(observed_addr.clone());
    SwarmPeer::from_identity(identity, addrs).map_err(HandshakeError::from)
}

/// Handshake protocol for Swarm peer authentication.
///
/// Implements the SYN-SYNACK-ACK exchange for mutual peer identity verification.
/// Used internally by `HandshakeUpgrade` — not a libp2p upgrade on its own.
pub(crate) struct HandshakeProtocol<I: SwarmIdentity> {
    identity: I,
    peer_id: PeerId,
    local_peer_id: Option<PeerId>,
    remote_addr: Multiaddr,
    additional_addrs: Vec<Multiaddr>,
}

impl<I: SwarmIdentity> HandshakeProtocol<I> {
    pub(crate) fn new(
        identity: I,
        peer_id: PeerId,
        remote_addr: Multiaddr,
        additional_addrs: Vec<Multiaddr>,
    ) -> Self {
        Self {
            identity,
            peer_id,
            local_peer_id: None,
            remote_addr,
            additional_addrs,
        }
    }

    /// Set the local peer ID for observed address validation.
    pub(crate) fn with_local_peer_id(mut self, local_peer_id: PeerId) -> Self {
        self.local_peer_id = Some(local_peer_id);
        self
    }

    #[instrument(
        name = "handshake",
        skip(self, stream),
        fields(
            direction = "inbound",
            peer_id = %self.peer_id,
            remote_addr = %self.remote_addr,
            remote_overlay = tracing::field::Empty,
        )
    )]
    pub(crate) async fn handle_inbound(self, stream: Stream) -> Result<HandshakeInfo, HandshakeError> {
        let mut metrics = HandshakeMetrics::inbound();
        metrics.initiated();
        let result = self.do_inbound_exchange(stream, &mut metrics).await;
        if let Ok(ref info) = result {
            Span::current().record("remote_overlay", tracing::field::display(info.swarm_peer.overlay()));
        }
        metrics.record(&result);
        result
    }

    async fn do_inbound_exchange(
        self,
        stream: Stream,
        metrics: &mut HandshakeMetrics,
    ) -> Result<HandshakeInfo, HandshakeError> {
        use vertex_swarm_net_proto::handshake::Ack;
        type Syn = vertex_swarm_net_proto::handshake::Syn;

        let network_id = self.identity.spec().network_id();

        // Receive SYN: peer tells us what address they see us at.
        let (syn, stream) = Framed::recv::<Syn, HandshakeError, _>(stream)
            .instrument(debug_span!("recv_syn"))
            .await?;
        let observed_multiaddr = decode_syn(syn)?;
        metrics.syn_exchanged();

        if let Some(local_peer_id) = &self.local_peer_id {
            validate_observed_addr(&observed_multiaddr, local_peer_id)?;
        }

        let local_peer =
            prepare_local_peer(&self.identity, &self.additional_addrs, &observed_multiaddr)?;

        // Send SYNACK: echo their observed addr back + our identity.
        let synack = encode_synack(
            &observed_multiaddr,
            &local_peer,
            self.identity.node_type(),
            self.identity.welcome_message().unwrap_or_default(),
            network_id,
        );
        let stream = Framed::send::<_, HandshakeError, _>(stream, synack)
            .instrument(debug_span!("send_synack"))
            .await?;
        metrics.synack_exchanged();

        // Receive ACK: peer's identity.
        let (ack, mut stream) = Framed::recv::<Ack, HandshakeError, _>(stream)
            .instrument(debug_span!("recv_ack"))
            .await?;
        let (swarm_peer, node_type, welcome_message) = decode_ack(ack, network_id)?;

        futures::AsyncWriteExt::close(&mut stream).await?;

        Ok(HandshakeInfo {
            peer_id: self.peer_id,
            swarm_peer,
            node_type,
            welcome_message,
            observed_multiaddr,
        })
    }

    #[instrument(
        name = "handshake",
        skip(self, stream),
        fields(
            direction = "outbound",
            peer_id = %self.peer_id,
            remote_addr = %self.remote_addr,
            remote_overlay = tracing::field::Empty,
        )
    )]
    pub(crate) async fn handle_outbound(self, stream: Stream) -> Result<HandshakeInfo, HandshakeError> {
        let mut metrics = HandshakeMetrics::outbound();
        metrics.initiated();
        let result = self.do_outbound_exchange(stream, &mut metrics).await;
        if let Ok(ref info) = result {
            Span::current().record("remote_overlay", tracing::field::display(info.swarm_peer.overlay()));
        }
        metrics.record(&result);
        result
    }

    async fn do_outbound_exchange(
        self,
        stream: Stream,
        metrics: &mut HandshakeMetrics,
    ) -> Result<HandshakeInfo, HandshakeError> {
        use vertex_swarm_net_proto::handshake::SynAck;

        let network_id = self.identity.spec().network_id();

        // Build the observed address we'll report to the remote peer.
        let mut their_observed_multiaddr = self.remote_addr.clone();
        // Strip existing /p2p/ suffix to prevent duplication (libp2p dial addresses include it).
        if matches!(their_observed_multiaddr.iter().last(), Some(Protocol::P2p(_))) {
            their_observed_multiaddr.pop();
        }
        let their_observed_multiaddr =
            their_observed_multiaddr.with(Protocol::P2p(self.peer_id));

        // Send SYN: tell peer what address we see them at.
        let stream = Framed::send::<_, HandshakeError, _>(stream, encode_syn(&their_observed_multiaddr))
            .instrument(debug_span!("send_syn"))
            .await?;
        metrics.syn_exchanged();

        // Receive SYNACK: peer echoes our observed addr + their identity.
        let (synack, stream) = Framed::recv::<SynAck, HandshakeError, _>(stream)
            .instrument(debug_span!("recv_synack"))
            .await?;
        let (observed_multiaddr, swarm_peer, node_type, welcome_message) =
            decode_synack(synack, network_id)?;
        metrics.synack_exchanged();

        if let Some(local_peer_id) = &self.local_peer_id {
            validate_observed_addr(&observed_multiaddr, local_peer_id)?;
        }

        let local_peer =
            prepare_local_peer(&self.identity, &self.additional_addrs, &observed_multiaddr)?;

        // Send ACK: our identity.
        let ack = encode_ack(
            &local_peer,
            self.identity.node_type(),
            self.identity.welcome_message().unwrap_or_default(),
            network_id,
        );
        let mut stream = Framed::send::<_, HandshakeError, _>(stream, ack)
            .instrument(debug_span!("send_ack"))
            .await?;

        futures::AsyncWriteExt::close(&mut stream).await?;

        Ok(HandshakeInfo {
            peer_id: self.peer_id,
            swarm_peer,
            node_type,
            welcome_message,
            observed_multiaddr,
        })
    }
}
