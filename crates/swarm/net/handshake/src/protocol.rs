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

/// Select the addresses to put in our signed record for this peer, and report
/// whether we had to fall back to the (ephemeral) observed address.
///
/// `additional_addrs` are our already-scope-filtered advertised addresses. A
/// non-empty set is used as-is; the peer-observed address is never appended to
/// it, so the resulting record is peer-independent and can be cached and reused
/// across handshakes. The observed address is used only as a last resort, for
/// both directions, when we have nothing real to advertise; the returned bool
/// is `true` only in that case. See [`prepare_local_peer`].
///
/// The NAT-discovery role the inbound observed address used to serve (a
/// port-forwarded node learning its public address) is now covered by AutoNAT
/// v2 plus advertising confirmed-external addresses, so the observed address no
/// longer needs to enter the common-case record.
fn select_local_addrs(
    additional_addrs: &[Multiaddr],
    observed_addr: &Multiaddr,
) -> (Vec<Multiaddr>, bool) {
    let addrs: Vec<Multiaddr> = additional_addrs
        .iter()
        .filter(|a| *a != observed_addr)
        .cloned()
        .collect();

    // A non-empty advertised set is used as-is for both directions.
    if !addrs.is_empty() {
        return (addrs, false);
    }

    // Last resort: with nothing real to advertise the observed address keeps the
    // record non-empty for the >=1 requirement. Transient until AutoNAT v2 /
    // UPnP / a static NAT address confirms a real one.
    (vec![observed_addr.clone()], true)
}

/// Sign a last-resort `SwarmPeer` over the peer-observed address.
///
/// Used only when the behaviour cache produced no record because the advertised
/// address set was empty (a node with no listen, NAT, or confirmed-external
/// address). The common case signs once per address-set change in the behaviour
/// and reuses a cached, peer-independent record; this path keeps the handshake
/// alive when there is nothing else to advertise.
///
/// Both vertex and bee reject a signed record with zero multiaddrs
/// (`SwarmPeer::sign` / bee `ParseAddress`), so the observed address is included
/// to satisfy the non-empty requirement. Such an entry is transient: it is
/// superseded once AutoNAT v2 / UPnP confirms a real external address (newer
/// timestamp wins).
fn prepare_local_peer<I: SwarmIdentity>(
    identity: &I,
    observed_addr: &Multiaddr,
) -> Result<SwarmPeer, HandshakeError> {
    let (addrs, ephemeral_fallback) = select_local_addrs(&[], observed_addr);

    // A full (storer) node's record is gossiped network-wide, so it must carry a
    // real reachable address. Falling back to the ephemeral observed address
    // means this storer is unreachable and is about to advertise an address no
    // peer can dial. A light (client) node is never gossiped, so its fallback is
    // harmless and silent. The fallback self-heals once AutoNAT v2 / UPnP / a
    // static NAT address provides a real one.
    if ephemeral_fallback && identity.is_full_node() {
        warn!(
            observed = %observed_addr,
            "full node has no reachable address; advertising an ephemeral address that peers \
             cannot dial. Configure --network.nat-addr or enable AutoNAT v2 / UPnP."
        );
    }

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
    /// Pre-signed self record from the behaviour cache, reused byte-identically
    /// across handshakes with an unchanged advertised address set. `None` when
    /// the advertised set was empty; the exchange then signs a last-resort
    /// record over the peer-observed address via [`prepare_local_peer`].
    self_record: Option<SwarmPeer>,
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
        self_record: Option<SwarmPeer>,
        purpose: &'static str,
    ) -> Self {
        Self {
            identity,
            peer_id,
            local_peer_id: None,
            remote_addr,
            self_record,
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

        // Use the cached, peer-independent record when the behaviour produced
        // one; otherwise the advertised set was empty, so sign a last-resort
        // record over the address the peer observed.
        let local_peer = match self.self_record.clone() {
            Some(record) => record,
            None => prepare_local_peer(&self.identity, &observed_multiaddr)?,
        };

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

        // Use the cached, peer-independent record when the behaviour produced
        // one; otherwise the advertised set was empty, so sign a last-resort
        // record over the address the peer observed.
        let local_peer = match self.self_record.clone() {
            Some(record) => record,
            None => prepare_local_peer(&self.identity, &observed_multiaddr)?,
        };

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

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> Multiaddr {
        s.parse().expect("valid multiaddr")
    }

    #[test]
    fn inbound_uses_advertised_set_without_observed() {
        // A non-empty advertised set is used as-is. The observed address is no
        // longer appended on inbound: the record stays peer-independent so it can
        // be cached and reused across handshakes. AutoNAT v2 covers the
        // NAT-discovery role the inbound observed-append used to serve.
        let additional = vec![addr("/ip4/8.8.4.4/tcp/1634")];
        let observed = addr("/ip4/203.0.113.7/tcp/1634");
        let (out, fallback) = select_local_addrs(&additional, &observed);
        assert_eq!(out, vec![addr("/ip4/8.8.4.4/tcp/1634")]);
        assert!(!out.contains(&observed));
        assert!(!fallback);
    }

    #[test]
    fn does_not_append_ephemeral_observed() {
        // The observed address is never appended to a non-empty set, so an
        // ephemeral NAT source port cannot pollute the gossiped record.
        let additional = vec![addr("/ip4/8.8.4.4/tcp/1634")];
        let observed = addr("/ip4/203.0.113.7/tcp/54321");
        let (out, fallback) = select_local_addrs(&additional, &observed);
        assert_eq!(out, vec![addr("/ip4/8.8.4.4/tcp/1634")]);
        assert!(!out.contains(&observed));
        assert!(!fallback);
    }

    #[test]
    fn uses_observed_only_as_last_resort() {
        // With nothing real to advertise, the observed address keeps the record
        // non-empty so the handshake completes (a zero-multiaddr record is
        // rejected). The fallback flag is set so a full node can warn; the entry
        // is transient until AutoNAT v2 / UPnP confirms a real address.
        let observed = addr("/ip4/203.0.113.7/tcp/54321");
        let (out, fallback) = select_local_addrs(&[], &observed);
        assert_eq!(out, vec![observed]);
        assert!(fallback);
    }

    #[test]
    fn ipv6_advertised_set_used_without_observed() {
        // The policy is address-family-agnostic: a non-empty IPv6 advertised set
        // is used as-is and the observed address is not appended.
        let additional = vec![addr("/ip6/2606:4700:4700::1111/tcp/1634")];
        let observed = addr("/ip6/2001:4860:4860::8888/tcp/1634");
        let (out, fallback) = select_local_addrs(&additional, &observed);
        assert_eq!(out, vec![addr("/ip6/2606:4700:4700::1111/tcp/1634")]);
        assert!(!out.contains(&observed));
        assert!(!fallback);
    }

    #[test]
    fn ipv6_last_resort_sets_fallback() {
        let observed = addr("/ip6/2001:4860:4860::8888/tcp/54321");
        let (out, fallback) = select_local_addrs(&[], &observed);
        assert_eq!(out, vec![observed]);
        assert!(fallback);
    }

    #[test]
    fn observed_is_filtered_from_advertised_set() {
        // If the observed address is already in the advertised set it is filtered
        // out, and an otherwise-empty result falls back to the observed address.
        let observed = addr("/ip4/203.0.113.7/tcp/1634");
        let additional = vec![observed.clone()];
        let (out, fallback) = select_local_addrs(&additional, &observed);
        assert_eq!(out, vec![observed]);
        assert!(fallback);
    }
}
