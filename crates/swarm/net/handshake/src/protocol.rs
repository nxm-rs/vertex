use libp2p::multiaddr::Protocol;
use libp2p::{Multiaddr, PeerId, Stream};
use tracing::{Instrument, Span, debug_span, instrument, warn};
use vertex_net_codec::FramedProto;
use vertex_net_utils::extract_peer_id;
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_peer::{SwarmPeer, Timestamp};
use vertex_swarm_spec::SwarmSpec;

use crate::admission::{AdmissionDecision, ConnectionDirection};
use crate::codec::{decode_ack, decode_syn, decode_synack, encode_ack, encode_syn, encode_synack};
use crate::metrics::HandshakeMetrics;
use crate::{HandshakeError, HandshakeInfo, SharedAdmissionControl};

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

    let signer = identity.signer();
    SwarmPeer::sign(
        &*signer,
        addrs,
        identity.overlay_address(),
        identity.spec().network_id(),
        identity.nonce(),
        Timestamp::now(),
        None,
    )
    .map_err(HandshakeError::from)
}

/// Handshake protocol for Swarm peer authentication.
///
/// Implements the SYN, SYNACK, ACK exchange for mutual peer identity
/// verification. Used internally by `HandshakeUpgrade`; not a libp2p
/// upgrade on its own.
pub(crate) struct HandshakeProtocol<I: SwarmIdentity> {
    identity: I,
    peer_id: PeerId,
    local_peer_id: Option<PeerId>,
    remote_addr: Multiaddr,
    additional_addrs: Vec<Multiaddr>,
    /// Optional admission gate. When set, the protocol consults it as
    /// soon as the remote peer's identity is verified and aborts with
    /// [`HandshakeError::AdmissionRejected`] on a `Reject` decision.
    admission_control: Option<(SharedAdmissionControl, ConnectionDirection)>,
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
            admission_control: None,
            purpose,
        }
    }

    /// Set the local peer ID for observed address validation.
    pub(crate) fn with_local_peer_id(mut self, local_peer_id: PeerId) -> Self {
        self.local_peer_id = Some(local_peer_id);
        self
    }

    /// Install the admission gate for this exchange.
    ///
    /// `direction` is which side this end of the connection plays
    /// (`Inbound` received SYN, `Outbound` sent SYN). Routing uses it to
    /// decide whether the in-flight peer is already counted in capacity.
    pub(crate) fn with_admission_control(
        mut self,
        admission_control: SharedAdmissionControl,
        direction: ConnectionDirection,
    ) -> Self {
        self.admission_control = Some((admission_control, direction));
        self
    }

    /// Evaluate admission control if installed.
    fn evaluate_admission(&self, info: &HandshakeInfo) -> Result<(), HandshakeError> {
        let Some((ref ac, direction)) = self.admission_control else {
            return Ok(());
        };
        match ac.evaluate(info.swarm_peer.overlay(), info.node_type, direction) {
            AdmissionDecision::Accept => Ok(()),
            AdmissionDecision::Reject(reason) => Err(HandshakeError::AdmissionRejected(reason)),
        }
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

        // Echo what we observed about the dialer (their `remote_addr` as
        // libp2p reported it) back in the SYNACK; the dialer validates the
        // peer id of this address against its own. An earlier version sent
        // the responder's own observed address instead, which the dialer
        // always rejected because the peer ids could never match.
        let mut dialer_observed = self.remote_addr.clone();
        if extract_peer_id(&dialer_observed).is_some() {
            dialer_observed.pop();
        }
        let dialer_observed = dialer_observed.with(Protocol::P2p(self.peer_id));

        // Send SYNACK: our identity + the dialer's address as we observe it.
        let synack = encode_synack(
            &dialer_observed,
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

        let info = HandshakeInfo {
            peer_id: self.peer_id,
            swarm_peer,
            node_type,
            welcome_message,
            observed_multiaddr,
        };

        // Consult admission control now that the peer's identity is
        // verified. Note the protocol asymmetry: the outbound side has
        // already sent its ACK and closed its half of the stream by the
        // time we reach this point, so a reject here surfaces as
        // `AdmissionRejected` locally and a transport-level disconnect
        // on the remote (see the module docs on
        // [`crate::admission`]).
        self.evaluate_admission(&info)?;

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

        // Build the observed address we'll report to the remote peer.
        let mut their_observed_multiaddr = self.remote_addr.clone();
        // Strip existing /p2p/ suffix to prevent duplication (libp2p dial addresses include it).
        if extract_peer_id(&their_observed_multiaddr).is_some() {
            their_observed_multiaddr.pop();
        }
        let their_observed_multiaddr = their_observed_multiaddr.with(Protocol::P2p(self.peer_id));

        // Send SYN: tell peer what address we see them at.
        let stream =
            Framed::send::<_, HandshakeError, _>(stream, encode_syn(&their_observed_multiaddr))
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

        // Consult admission control before sending ACK so a reject
        // aborts cleanly without committing to the exchange.
        let info = HandshakeInfo {
            peer_id: self.peer_id,
            swarm_peer,
            node_type,
            welcome_message,
            observed_multiaddr,
        };
        self.evaluate_admission(&info)?;

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

        Ok(info)
    }
}
