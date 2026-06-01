//! Network driver for the typestate handshake.
//!
//! `protocol.rs` glues [`session::Handshake`] to an `AsyncRead + AsyncWrite`
//! stream and to the local [`SwarmIdentity`]. It owns no protocol state of
//! its own: every step lives in the typestate and the wire codec.

use std::time::{SystemTime, UNIX_EPOCH};

use libp2p::multiaddr::Protocol;
use libp2p::{Multiaddr, PeerId, Stream};
use tracing::{Instrument, Span, debug_span, instrument, warn};
use vertex_net_codec::FramedProto;
use vertex_net_utils::extract_peer_id;
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_peer::{BzzAddress, Nonce, Timestamp};
use vertex_swarm_spec::SwarmSpec;

use crate::codec::{encode_ack, encode_syn, encode_synack};
use crate::metrics::HandshakeMetrics;
use crate::session::{Handshake, Initiator, Responder, SessionInputs};
use crate::welcome::WelcomeMessage;
use crate::{HandshakeError, HandshakeInfo};

/// Maximum size for handshake message buffers.
const MAX_HANDSHAKE_BUFFER_SIZE: usize = 1024;

type Framed = FramedProto<MAX_HANDSHAKE_BUFFER_SIZE>;

/// Validate that observed address contains the expected peer ID.
fn validate_observed_addr(
    observed: &Multiaddr,
    expected_peer_id: &PeerId,
) -> Result<(), HandshakeError> {
    let peer_id_from_addr = extract_peer_id(observed);

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

/// Build a signed [`BzzAddress`] for the local identity using `observed` as
/// the highest-priority underlay (filtered out of `additional`, then
/// appended last). Mirrors bee's address-construction order.
fn build_local_bzz<I: SwarmIdentity + ?Sized>(
    identity: &I,
    additional: &[Multiaddr],
    observed: &Multiaddr,
    timestamp: Timestamp,
) -> Result<BzzAddress, HandshakeError> {
    let mut underlay: Vec<Multiaddr> = additional
        .iter()
        .filter(|a| *a != observed)
        .cloned()
        .collect();
    underlay.push(observed.clone());

    let signer = identity.signer();
    let nonce = Nonce::from(identity.nonce());
    let overlay = identity.overlay_address();
    let network_id = identity.spec().network_id();

    let bzz = BzzAddress::sign(
        signer.as_ref(),
        underlay,
        overlay,
        network_id,
        nonce,
        timestamp,
        None,
    )?;
    Ok(bzz)
}

/// `SystemTime::now()` in seconds since the epoch, as a [`Timestamp`].
///
/// Clamps to `[1, i64::MAX]`: the bee wire format requires strictly positive
/// timestamps, and the `u64 → i64` conversion is checked so a far-future clock
/// (past `i64::MAX` ≈ year 292277026596) yields the maximum instead of a
/// silently wrapped negative value.
fn now_timestamp() -> Timestamp {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(1);
    Timestamp::from_secs(secs.max(1))
}

/// Extract a [`WelcomeMessage`] from the identity, truncating to fit.
///
/// We use `truncated` on the local side because the identity is trusted and
/// we'd rather advertise *something* than fail the handshake over a too-long
/// configured message.
fn local_welcome<I: SwarmIdentity + ?Sized>(identity: &I) -> WelcomeMessage {
    WelcomeMessage::truncated(identity.welcome_message().unwrap_or(""))
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
    purpose: &'static str,
}

impl<I: SwarmIdentity> HandshakeProtocol<I> {
    pub(crate) fn new(
        identity: I,
        peer_id: PeerId,
        remote_addr: Multiaddr,
        additional_addrs: Vec<Multiaddr>,
        purpose: &'static str,
    ) -> Self {
        Self {
            identity,
            peer_id,
            local_peer_id: None,
            remote_addr,
            additional_addrs,
            purpose,
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
    pub(crate) async fn handle_inbound(
        self,
        stream: Stream,
    ) -> Result<HandshakeInfo, HandshakeError> {
        let mut metrics = HandshakeMetrics::inbound(self.purpose);
        metrics.initiated();
        let result = self.do_inbound_exchange(stream, &mut metrics).await;
        if let Ok(ref info) = result {
            Span::current().record(
                "remote_overlay",
                tracing::field::display(info.swarm_peer.overlay()),
            );
        }
        metrics.record(&result);
        result
    }

    async fn do_inbound_exchange(
        self,
        stream: Stream,
        metrics: &mut HandshakeMetrics,
    ) -> Result<HandshakeInfo, HandshakeError> {
        use vertex_swarm_net_proto::handshake::{Ack, Syn};

        let network_id = self.identity.spec().network_id();
        let now = now_timestamp();

        let inputs = SessionInputs {
            network_id,
            peer_id: self.peer_id,
            remote_addr: self.remote_addr.clone(),
            welcome_message: local_welcome(&self.identity),
            node_type: self.identity.node_type(),
            now: Some(now),
        };
        let session = Handshake::<Responder, crate::session::Awaiting>::responder(inputs);

        // Receive SYN: peer tells us what address they see us at.
        let (syn, stream) = Framed::recv::<Syn, HandshakeError, _>(stream)
            .instrument(debug_span!("recv_syn"))
            .await?;
        let observed_multiaddr = crate::codec::decode_syn(syn)?;
        metrics.syn_exchanged();

        if let Some(local_peer_id) = &self.local_peer_id {
            validate_observed_addr(&observed_multiaddr, local_peer_id)?;
        }

        let session = session.syn_received(observed_multiaddr.clone());

        let local_bzz = build_local_bzz(
            &self.identity,
            &self.additional_addrs,
            &observed_multiaddr,
            now,
        )?;

        // Send SYNACK: echo their observed addr back + our identity.
        let welcome = local_welcome(&self.identity);
        let synack = encode_synack(
            &observed_multiaddr,
            &local_bzz,
            self.identity.node_type(),
            &welcome,
            network_id,
        );
        let stream = Framed::send::<_, HandshakeError, _>(stream, synack)
            .instrument(debug_span!("send_synack"))
            .await?;
        let session = session.synack_sent();
        metrics.synack_exchanged();

        // Receive ACK: peer's identity.
        let (ack, mut stream) = Framed::recv::<Ack, HandshakeError, _>(stream)
            .instrument(debug_span!("recv_ack"))
            .await?;
        let decoded = crate::codec::decode_ack(ack, network_id, Some(now))?;

        let info = session.ack_received(decoded).map_err(|e| e.error)?;

        futures::AsyncWriteExt::close(&mut stream).await?;
        Ok(info)
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
    pub(crate) async fn handle_outbound(
        self,
        stream: Stream,
    ) -> Result<HandshakeInfo, HandshakeError> {
        let mut metrics = HandshakeMetrics::outbound(self.purpose);
        metrics.initiated();
        let result = self.do_outbound_exchange(stream, &mut metrics).await;
        if let Ok(ref info) = result {
            Span::current().record(
                "remote_overlay",
                tracing::field::display(info.swarm_peer.overlay()),
            );
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
        let now = now_timestamp();

        // Build the observed address we'll report to the remote peer.
        let mut their_observed_multiaddr = self.remote_addr.clone();
        // Strip existing /p2p/ suffix to prevent duplication (libp2p dial
        // addresses include it).
        if extract_peer_id(&their_observed_multiaddr).is_some() {
            their_observed_multiaddr.pop();
        }
        let their_observed_multiaddr = their_observed_multiaddr.with(Protocol::P2p(self.peer_id));

        let inputs = SessionInputs {
            network_id,
            peer_id: self.peer_id,
            remote_addr: self.remote_addr.clone(),
            welcome_message: local_welcome(&self.identity),
            node_type: self.identity.node_type(),
            now: Some(now),
        };
        let session = Handshake::<Initiator, crate::session::Init>::initiator(inputs);

        // Send SYN: tell peer what address we see them at.
        let stream =
            Framed::send::<_, HandshakeError, _>(stream, encode_syn(&their_observed_multiaddr))
                .instrument(debug_span!("send_syn"))
                .await?;
        let session = session.syn_sent();
        metrics.syn_exchanged();

        // Receive SYNACK: peer echoes our observed addr + their identity.
        let (synack, stream) = Framed::recv::<SynAck, HandshakeError, _>(stream)
            .instrument(debug_span!("recv_synack"))
            .await?;
        let decoded = crate::codec::decode_synack(synack, network_id, Some(now))?;
        metrics.synack_exchanged();

        if let Some(local_peer_id) = &self.local_peer_id {
            validate_observed_addr(&decoded.observed_multiaddr, local_peer_id)?;
        }

        let observed_by_remote = decoded.observed_multiaddr.clone();
        let session = session.synack_received(decoded).map_err(|e| e.error)?;

        let local_bzz = build_local_bzz(
            &self.identity,
            &self.additional_addrs,
            &observed_by_remote,
            now,
        )?;

        // Send ACK: our identity.
        let welcome = local_welcome(&self.identity);
        let ack = encode_ack(&local_bzz, self.identity.node_type(), &welcome, network_id);
        let mut stream = Framed::send::<_, HandshakeError, _>(stream, ack)
            .instrument(debug_span!("send_ack"))
            .await?;

        let info = session.complete(local_bzz).map_err(|e| e.error)?;

        futures::AsyncWriteExt::close(&mut stream).await?;
        Ok(info)
    }
}
