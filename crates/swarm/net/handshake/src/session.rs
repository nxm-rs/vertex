//! Typestate session model for the handshake exchange.
//!
//! The session is parameterised on a role marker (`Initiator` or `Responder`)
//! and a phase marker. Phase transitions are *consuming-self*: each step
//! returns a new typestate value whose marker reflects the next legal phase.
//! This makes "send Ack before SYN" or "receive SynAck twice" outright
//! uncompilable.
//!
//! The session itself is pure — it owns the protocol *data* and the local
//! identity inputs, but does no I/O. `protocol.rs` glues the typestate to an
//! `AsyncRead + AsyncWrite` stream by stepping the phases as bytes arrive.
//!
//! ```text
//! Initiator: Init ─send Syn─▶ SynSent ─recv SynAck─▶ AckReceived ─send Ack─▶ Complete
//! Responder: Awaiting ─recv Syn─▶ SynReceived ─send SynAck─▶ SynAckSent ─recv Ack─▶ Complete
//! ```
//!
//! Errors carry the consumed prior state so callers can rebuild the session
//! without re-deriving inputs.

use std::marker::PhantomData;

use libp2p::Multiaddr;
use vertex_swarm_peer::{BzzAddress, SwarmNodeType, Timestamp};

use crate::codec::{DecodedAck, DecodedSynAck};
use crate::welcome::WelcomeMessage;
use crate::{HandshakeError, HandshakeInfo};

/// Role marker for the initiator side of the exchange.
#[derive(Debug)]
pub enum Initiator {}
/// Role marker for the responder side of the exchange.
#[derive(Debug)]
pub enum Responder {}

/// Sealed trait identifying a role marker.
pub trait Role: private::Sealed {}
impl Role for Initiator {}
impl Role for Responder {}

/// Phase marker: the initiator has not sent anything yet.
#[derive(Debug)]
pub enum Init {}
/// Phase marker: the initiator has sent its SYN and is awaiting SynAck.
#[derive(Debug)]
pub enum SynSent {}
/// Phase marker: the initiator has received the responder's SynAck.
///
/// The plan refers to this as `AckReceived` because the inner `Ack` is the
/// authoritative identity payload.
#[derive(Debug)]
pub enum AckReceived {}

/// Phase marker: the responder is waiting for the initiator's SYN.
#[derive(Debug)]
pub enum Awaiting {}
/// Phase marker: the responder has received SYN, but not yet sent SynAck.
#[derive(Debug)]
pub enum SynReceived {}
/// Phase marker: the responder has sent SynAck and is awaiting the Ack.
#[derive(Debug)]
pub enum SynAckSent {}

/// Phase marker: the exchange has produced a verified [`HandshakeInfo`].
#[derive(Debug)]
pub enum Complete {}

/// Sealed trait identifying a phase marker.
pub trait Phase: private::Sealed {}
impl Phase for Init {}
impl Phase for SynSent {}
impl Phase for AckReceived {}
impl Phase for Awaiting {}
impl Phase for SynReceived {}
impl Phase for SynAckSent {}
impl Phase for Complete {}

mod private {
    pub trait Sealed {}
    impl Sealed for super::Initiator {}
    impl Sealed for super::Responder {}
    impl Sealed for super::Init {}
    impl Sealed for super::SynSent {}
    impl Sealed for super::AckReceived {}
    impl Sealed for super::Awaiting {}
    impl Sealed for super::SynReceived {}
    impl Sealed for super::SynAckSent {}
    impl Sealed for super::Complete {}
}

/// Common inputs that every session phase needs regardless of role.
///
/// The session itself does not perform any encoding — `welcome_message`,
/// `node_type` and the local address are surfaced here so the protocol
/// driver (or tests) can read back the values the local side intends to
/// advertise. The driver is responsible for encoding them into the wire
/// message; the session only tracks the *remote* identity it has verified.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SessionInputs {
    /// Network id we expect to see on the wire.
    pub network_id: u64,
    /// libp2p peer id of the remote party.
    pub peer_id: libp2p::PeerId,
    /// Locally observed remote multiaddr (used to echo back during SYN).
    pub remote_addr: Multiaddr,
    /// Welcome message the local side intends to advertise.
    pub welcome_message: WelcomeMessage,
    /// Node type the local side will advertise to the peer.
    pub node_type: SwarmNodeType,
    /// "Now" used for skew validation when receiving an Ack. `None` opts out.
    pub now: Option<Timestamp>,
}

/// A handshake session at phase `S` for role `R`.
///
/// Constructed with [`Handshake::initiator`] / [`Handshake::responder`].
/// Each step consumes `self` and returns a new typestate; on failure the
/// `PhaseError` carries the same `self`-state-before-the-transition so the
/// caller can decide whether to retry, log, or close the stream.
pub struct Handshake<R: Role, S: Phase> {
    inputs: SessionInputs,
    state: SessionState,
    _role: PhantomData<fn() -> R>,
    _phase: PhantomData<fn() -> S>,
}

impl<R: Role, S: Phase> std::fmt::Debug for Handshake<R, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Handshake")
            .field("role", &std::any::type_name::<R>())
            .field("phase", &std::any::type_name::<S>())
            .field("network_id", &self.inputs.network_id)
            .field("peer_id", &self.inputs.peer_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default, Clone)]
struct SessionState {
    /// Remote-side data collected during the exchange.
    remote_bzz: Option<BzzAddress>,
    remote_ethereum: Option<alloy_primitives::Address>,
    remote_node_type: Option<SwarmNodeType>,
    remote_welcome: Option<WelcomeMessage>,
    /// Address we observe at the peer (only for the initiator we echo this).
    observed_by_remote: Option<Multiaddr>,
}

/// Error returned when a phase transition cannot proceed.
///
/// Carries the prior typed state so retry/recovery logic can rebuild without
/// re-deriving inputs from scratch.
#[non_exhaustive]
pub struct PhaseError<R: Role, S: Phase> {
    /// Underlying handshake error.
    pub error: HandshakeError,
    /// State the session was in *before* the failed transition.
    pub prior: Handshake<R, S>,
}

impl<R: Role, S: Phase> std::fmt::Debug for PhaseError<R, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhaseError")
            .field("error", &self.error)
            .field("prior", &self.prior)
            .finish()
    }
}

impl<R: Role, S: Phase> std::fmt::Display for PhaseError<R, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error)
    }
}

impl<R: Role, S: Phase> std::error::Error for PhaseError<R, S> {}

impl<R: Role, S: Phase> Handshake<R, S> {
    /// Borrow the session inputs.
    pub fn inputs(&self) -> &SessionInputs {
        &self.inputs
    }

    /// Helper for the protocol layer to convert this transition into a
    /// plain `HandshakeError`, dropping the prior state.
    fn into_err(self, error: HandshakeError) -> Box<PhaseError<R, S>> {
        Box::new(PhaseError { error, prior: self })
    }
}

// ─── Initiator state machine ─────────────────────────────────────────────────

/// Constructors and initial-phase transitions.
impl Handshake<Initiator, Init> {
    /// Build a fresh initiator session.
    pub fn initiator(inputs: SessionInputs) -> Self {
        Self {
            inputs,
            state: SessionState::default(),
            _role: PhantomData,
            _phase: PhantomData,
        }
    }

    /// Mark the SYN as sent. The actual encode/write happens in `protocol.rs`.
    pub fn syn_sent(self) -> Handshake<Initiator, SynSent> {
        Handshake {
            inputs: self.inputs,
            state: self.state,
            _role: PhantomData,
            _phase: PhantomData,
        }
    }
}

impl Handshake<Initiator, SynSent> {
    /// Consume a decoded SynAck and advance.
    pub fn synack_received(
        mut self,
        decoded: DecodedSynAck,
    ) -> Result<Handshake<Initiator, AckReceived>, Box<PhaseError<Initiator, SynSent>>> {
        // The Ack inside SynAck is the canonical remote identity.
        let DecodedAck {
            bzz_address,
            ethereum_address,
            node_type,
            welcome_message,
        } = decoded.ack;
        self.state.observed_by_remote = Some(decoded.observed_multiaddr);
        self.state.remote_bzz = Some(bzz_address);
        self.state.remote_ethereum = Some(ethereum_address);
        self.state.remote_node_type = Some(node_type);
        self.state.remote_welcome = Some(welcome_message);
        Ok(Handshake {
            inputs: self.inputs,
            state: self.state,
            _role: PhantomData,
            _phase: PhantomData,
        })
    }
}

impl Handshake<Initiator, AckReceived> {
    /// The address the remote peer observed us at (echoed back in SynAck).
    ///
    /// Returns `None` only if the session was constructed manually and bypassed
    /// the typestate invariant. The public constructors never produce that
    /// state.
    pub fn observed_by_remote(&self) -> Option<&Multiaddr> {
        self.state.observed_by_remote.as_ref()
    }

    /// Finalise the exchange once our own Ack has been written.
    ///
    /// `_local_bzz` is accepted for API symmetry with the responder side and
    /// to document that the caller has already encoded the local address; the
    /// session does not embed it in the returned [`HandshakeInfo`] because
    /// `HandshakeInfo` describes the *remote* peer.
    pub fn complete(
        self,
        _local_bzz: BzzAddress,
    ) -> Result<HandshakeInfo, Box<PhaseError<Initiator, AckReceived>>> {
        let observed = match self.state.observed_by_remote.clone() {
            Some(o) => o,
            None => {
                return Err(self.into_err(HandshakeError::MissingField("observed_multiaddr")));
            }
        };
        let bzz = match self.state.remote_bzz.clone() {
            Some(b) => b,
            None => {
                return Err(self.into_err(HandshakeError::MissingField("remote_bzz")));
            }
        };
        let ethereum = self.state.remote_ethereum.unwrap_or_default();
        Ok(build_info(
            self.inputs.peer_id,
            bzz,
            ethereum,
            self.state.remote_node_type.unwrap_or(SwarmNodeType::Client),
            self.state.remote_welcome.clone().unwrap_or_default(),
            observed,
        ))
    }
}

// ─── Responder state machine ─────────────────────────────────────────────────

impl Handshake<Responder, Awaiting> {
    /// Build a fresh responder session.
    pub fn responder(inputs: SessionInputs) -> Self {
        Self {
            inputs,
            state: SessionState::default(),
            _role: PhantomData,
            _phase: PhantomData,
        }
    }

    /// Record the SYN's observed multiaddr and advance.
    pub fn syn_received(
        mut self,
        observed_by_remote: Multiaddr,
    ) -> Handshake<Responder, SynReceived> {
        self.state.observed_by_remote = Some(observed_by_remote);
        Handshake {
            inputs: self.inputs,
            state: self.state,
            _role: PhantomData,
            _phase: PhantomData,
        }
    }
}

impl Handshake<Responder, SynReceived> {
    /// The address the remote peer observed us at.
    ///
    /// Returns `None` only if the session was constructed manually and bypassed
    /// the typestate invariant. The public constructors never produce that
    /// state.
    pub fn observed_by_remote(&self) -> Option<&Multiaddr> {
        self.state.observed_by_remote.as_ref()
    }

    /// Mark our SynAck as sent and transition.
    pub fn synack_sent(self) -> Handshake<Responder, SynAckSent> {
        Handshake {
            inputs: self.inputs,
            state: self.state,
            _role: PhantomData,
            _phase: PhantomData,
        }
    }
}

impl Handshake<Responder, SynAckSent> {
    /// Consume the peer's Ack and produce a verified [`HandshakeInfo`].
    pub fn ack_received(
        self,
        decoded: DecodedAck,
    ) -> Result<HandshakeInfo, Box<PhaseError<Responder, SynAckSent>>> {
        let observed = match self.state.observed_by_remote.clone() {
            Some(o) => o,
            None => {
                return Err(self.into_err(HandshakeError::MissingField("observed_multiaddr")));
            }
        };
        let DecodedAck {
            bzz_address,
            ethereum_address,
            node_type,
            welcome_message,
        } = decoded;
        Ok(build_info(
            self.inputs.peer_id,
            bzz_address,
            ethereum_address,
            node_type,
            welcome_message,
            observed,
        ))
    }
}

fn build_info(
    peer_id: libp2p::PeerId,
    bzz: BzzAddress,
    ethereum_address: alloy_primitives::Address,
    node_type: SwarmNodeType,
    welcome_message: WelcomeMessage,
    observed_multiaddr: Multiaddr,
) -> HandshakeInfo {
    // Project BzzAddress → legacy SwarmPeer for downstream consumers.
    // `ethereum_address` was recovered (and verified) by the codec layer; we
    // pass it through rather than re-running EIP-191 recovery.
    let swarm_peer = vertex_swarm_peer::SwarmPeer::from_validated(
        bzz.underlay().to_vec(),
        *bzz.signature(),
        (*bzz.overlay()).into(),
        (*bzz.nonce()).into(),
        ethereum_address,
    );
    HandshakeInfo {
        peer_id,
        bzz_address: bzz,
        swarm_peer,
        node_type,
        welcome_message,
        observed_multiaddr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_signer_local::PrivateKeySigner;
    use libp2p::PeerId;
    use vertex_swarm_peer::Nonce;
    use vertex_swarm_primitives::compute_overlay;

    use crate::codec::{decode_ack, decode_synack, encode_ack, encode_synack};

    fn fixed_now() -> Timestamp {
        Timestamp::from_secs(1_700_000_000)
    }

    fn inputs(peer_id: PeerId, remote_addr: Multiaddr, network_id: u64) -> SessionInputs {
        SessionInputs {
            network_id,
            peer_id,
            remote_addr,
            welcome_message: WelcomeMessage::new("local").unwrap(),
            node_type: SwarmNodeType::Storer,
            now: Some(fixed_now()),
        }
    }

    fn signed_bzz(network_id: u64) -> (BzzAddress, PrivateKeySigner) {
        let signer = PrivateKeySigner::random();
        let nonce = Nonce::from([0xAAu8; 32]);
        let overlay = compute_overlay(&signer.address(), network_id, nonce.as_b256());
        let underlay: Vec<Multiaddr> = vec!["/ip4/10.0.0.1/tcp/4242".parse().unwrap()];
        let bzz = BzzAddress::sign(
            &signer,
            underlay,
            overlay,
            network_id,
            nonce,
            fixed_now(),
            None,
        )
        .unwrap();
        (bzz, signer)
    }

    #[test]
    fn initiator_phase_progression() {
        let peer_id = PeerId::random();
        let remote: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();
        let (remote_bzz, _) = signed_bzz(1);

        let hs = Handshake::<Initiator, Init>::initiator(inputs(peer_id, remote.clone(), 1));
        let hs = hs.syn_sent();

        // Build a SynAck the initiator would see on the wire.
        let observed: Multiaddr = "/ip4/8.8.8.8/tcp/9999".parse().unwrap();
        let proto = encode_synack(
            &observed,
            &remote_bzz,
            SwarmNodeType::Storer,
            &WelcomeMessage::new("remote").unwrap(),
            1,
        );
        let decoded = decode_synack(proto, 1, Some(fixed_now())).unwrap();
        let hs = hs.synack_received(decoded).unwrap();
        assert_eq!(hs.observed_by_remote(), Some(&observed));

        let (local_bzz, _) = signed_bzz(1);
        let info = hs.complete(local_bzz).unwrap();
        assert_eq!(info.peer_id, peer_id);
        assert_eq!(info.bzz_address, remote_bzz);
        assert_eq!(info.observed_multiaddr, observed);
    }

    #[test]
    fn responder_phase_progression() {
        let peer_id = PeerId::random();
        let remote: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();
        let (remote_bzz, _) = signed_bzz(1);

        let hs = Handshake::<Responder, Awaiting>::responder(inputs(peer_id, remote.clone(), 1));
        let observed: Multiaddr = "/ip4/1.1.1.1/tcp/443".parse().unwrap();
        let hs = hs.syn_received(observed.clone());
        assert_eq!(hs.observed_by_remote(), Some(&observed));
        let hs = hs.synack_sent();

        let ack_proto = encode_ack(
            &remote_bzz,
            SwarmNodeType::Client,
            &WelcomeMessage::new("remote").unwrap(),
            1,
        );
        let decoded = decode_ack(ack_proto, 1, Some(fixed_now())).unwrap();
        let info = hs.ack_received(decoded).unwrap();
        assert_eq!(info.peer_id, peer_id);
        assert_eq!(info.bzz_address, remote_bzz);
        assert_eq!(info.observed_multiaddr, observed);
        assert_eq!(info.node_type, SwarmNodeType::Client);
    }

    #[test]
    fn initiator_recovers_known_keypair() {
        // Drive the typestate end-to-end using a fixed signer to exercise the
        // recovery path against a known address.
        let peer_id = PeerId::random();
        let remote: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();
        let raw = alloy_primitives::B256::repeat_byte(0x33);
        let signer = PrivateKeySigner::from_bytes(&raw).unwrap();
        let nonce = Nonce::from([0x77u8; 32]);
        let overlay = compute_overlay(&signer.address(), 1, nonce.as_b256());
        let underlay: Vec<Multiaddr> = vec!["/ip4/10.0.0.1/tcp/4242".parse().unwrap()];
        let remote_bzz =
            BzzAddress::sign(&signer, underlay, overlay, 1, nonce, fixed_now(), None).unwrap();

        let hs = Handshake::<Initiator, Init>::initiator(inputs(peer_id, remote, 1)).syn_sent();
        let observed: Multiaddr = "/ip4/8.8.8.8/tcp/9999".parse().unwrap();
        let proto = encode_synack(
            &observed,
            &remote_bzz,
            SwarmNodeType::Storer,
            &WelcomeMessage::empty(),
            1,
        );
        let decoded = decode_synack(proto, 1, Some(fixed_now())).unwrap();
        let hs = hs.synack_received(decoded).unwrap();
        let (local_bzz, _) = signed_bzz(1);
        let info = hs.complete(local_bzz).unwrap();
        assert_eq!(info.swarm_peer.ethereum_address(), &signer.address());
    }
}
