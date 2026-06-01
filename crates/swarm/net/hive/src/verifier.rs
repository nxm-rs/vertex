//! Per-record verification for inbound hive 2.0.0 peer exchanges.
//!
//! Refactors what used to be inline closure logic inside [`crate::protocol`]
//! into a small object-safe [`HiveVerifier`] trait so future work (Unit 7's
//! bootnode discard gate, Unit 8's saturation-aware admission, network-id
//! mismatch in fuzz harnesses, etc.) can swap policy without forking the
//! parsing path.
//!
//! Each verifier MUST be able to reject a record with a stable
//! [`HiveRejection`] variant — every variant doubles as a metric label.

use std::sync::Arc;

use parking_lot::Mutex;
use tracing::debug;
use vertex_swarm_peer::{SwarmAddress, SwarmPeer};
use vertex_swarm_primitives::compute_overlay;

use crate::bzz::{BzzAddress, BzzAddressError, check_timestamp_skew};

/// Where an inbound peer record was received from.
///
/// Currently every received hive record is `Gossip`; the variant exists so
/// future protocols (e.g. handshake addrbook reuse) can share the verifier
/// shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GossipSource {
    /// Record was learned from a hive `Peers` broadcast.
    Gossip,
}

/// A peer record that passed verification.
#[derive(Debug, Clone)]
pub struct VerifiedPeer {
    /// The verified peer in vertex's canonical form.
    pub peer: SwarmPeer,
}

/// Why a peer record was rejected. Every variant is a stable metric label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum HiveRejection {
    /// Signature failed to recover, or recovered to an address that does not
    /// own the claimed overlay (re-derivation check failed).
    InvalidSignature,
    /// Timestamp is zero/negative or more than [`crate::bzz::MAX_CLOCK_SKEW_SECS`]
    /// seconds in the future.
    TimestampOutsideSkewWindow,
    /// Sender's claimed network id does not match ours.
    NetworkIdMismatch,
    /// Peer is on a local blocklist (reserved for callers that supply one;
    /// the default verifier never emits this unless a blocklist is attached).
    Blocklisted,
    /// Underlay multiaddrs decoded to a private/loopback/link-local-only set
    /// that the local policy refuses to keep.
    PrivateMultiaddr,
    /// Record's structural invariants failed (nonce/overlay/chequebook length,
    /// missing underlays).
    Malformed,
    /// Sender gossiped its own overlay back to us. Bee filters this out
    /// before broadcasting; rejecting it here defends against self-dial.
    SelfOverlay,
}

impl core::fmt::Display for HiveRejection {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(<&'static str>::from(*self))
    }
}

impl std::error::Error for HiveRejection {}

impl From<BzzAddressError> for HiveRejection {
    fn from(err: BzzAddressError) -> Self {
        match err {
            BzzAddressError::SignatureFormat => Self::InvalidSignature,
            BzzAddressError::TimestampInvalid => Self::TimestampOutsideSkewWindow,
            _ => Self::Malformed,
        }
    }
}

/// Verification surface for inbound hive peer records.
///
/// Implementations MUST be cheap to call concurrently — the verifier is
/// invoked once per gossiped record on the validation hot path.
pub trait HiveVerifier: Send + Sync {
    /// Inspect a parsed [`BzzAddress`] and return either a [`VerifiedPeer`]
    /// or a [`HiveRejection`] suitable for metric labelling.
    fn verify(
        &self,
        addr: &BzzAddress,
        source: GossipSource,
    ) -> Result<VerifiedPeer, HiveRejection>;
}

/// Wall-clock source used by [`DefaultHiveVerifier`].
///
/// Indirected through a trait object so tests can pin time without depending
/// on `tokio::time::pause`.
type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Tiny query interface so the verifier doesn't depend on a concrete
/// blocklist crate. Implement on any cache that can answer "is this overlay
/// banned right now?".
pub trait BlocklistQuery: Send + 'static {
    fn is_blocked(&mut self, overlay: &SwarmAddress) -> bool;
}

/// Default hive verifier: validates signature, re-derives overlay, and checks
/// the timestamp against the local clock.
///
/// Cheap to clone (everything is behind `Arc`).
#[derive(Clone)]
pub struct DefaultHiveVerifier {
    network_id: u64,
    local_overlay: SwarmAddress,
    now_secs: Clock,
    blocklist: Option<Arc<Mutex<dyn BlocklistQuery>>>,
}

impl DefaultHiveVerifier {
    /// Create a verifier backed by the real wall clock.
    pub fn new(network_id: u64, local_overlay: SwarmAddress) -> Self {
        Self {
            network_id,
            local_overlay,
            now_secs: Arc::new(default_now_secs),
            blocklist: None,
        }
    }

    /// Create a verifier with a custom clock source (used by tests).
    pub fn with_clock<F>(network_id: u64, local_overlay: SwarmAddress, now_secs: F) -> Self
    where
        F: Fn() -> i64 + Send + Sync + 'static,
    {
        Self {
            network_id,
            local_overlay,
            now_secs: Arc::new(now_secs),
            blocklist: None,
        }
    }

    /// Attach a blocklist query — when present, the verifier rejects any
    /// overlay the blocklist marks as blocked with [`HiveRejection::Blocklisted`].
    pub fn with_blocklist<B: BlocklistQuery>(mut self, blocklist: Arc<Mutex<B>>) -> Self {
        self.blocklist = Some(blocklist as Arc<Mutex<dyn BlocklistQuery>>);
        self
    }
}

fn default_now_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl HiveVerifier for DefaultHiveVerifier {
    fn verify(
        &self,
        addr: &BzzAddress,
        _source: GossipSource,
    ) -> Result<VerifiedPeer, HiveRejection> {
        // Self-overlay defence — never re-admit our own address.
        if addr.overlay == self.local_overlay {
            return Err(HiveRejection::SelfOverlay);
        }

        // Timestamp skew window.
        let now = (self.now_secs)();
        if !check_timestamp_skew(addr.timestamp, now) {
            return Err(HiveRejection::TimestampOutsideSkewWindow);
        }

        // Optional blocklist short-circuit (cheaper than ECDSA recovery).
        if let Some(blocklist) = &self.blocklist
            && blocklist.lock().is_blocked(&addr.overlay)
        {
            return Err(HiveRejection::Blocklisted);
        }

        // Signature recovery + overlay re-derivation.
        let signer = addr
            .verify_signature(self.network_id)
            .map_err(|_| HiveRejection::InvalidSignature)?;

        let expected_overlay = compute_overlay(&signer, self.network_id, &addr.nonce);
        if expected_overlay != addr.overlay {
            debug!(
                claimed = %addr.overlay,
                derived = %expected_overlay,
                "Hive: overlay does not derive from recovered signer"
            );
            return Err(HiveRejection::InvalidSignature);
        }

        let peer = addr.clone().into_swarm_peer(signer);
        Ok(VerifiedPeer { peer })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "tests")]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256};
    use std::sync::Arc;
    use vertex_swarm_api::{SwarmIdentity, SwarmSpec};
    use vertex_swarm_identity::Identity;
    use vertex_swarm_primitives::SwarmNodeType;
    use vertex_swarm_spec::init_testnet;

    /// Build a fully-signed `BzzAddress` for the given identity at the given
    /// timestamp. Signs using the bee 15.0.0 layout (with zero chequebook).
    fn signed_bzz_address(
        identity: &Identity,
        underlays: Vec<libp2p::Multiaddr>,
        timestamp: i64,
    ) -> BzzAddress {
        use alloy_signer::SignerSync;
        use vertex_swarm_peer::serialize_multiaddrs;

        let underlay_bytes = serialize_multiaddrs(&underlays);
        let overlay = identity.overlay_address();
        let nonce = identity.nonce();
        let network_id = identity.spec().network_id();
        let chequebook = Address::ZERO;

        let msg = crate::bzz::generate_sign_data(
            &underlay_bytes,
            overlay.as_ref(),
            network_id,
            nonce.as_slice(),
            timestamp,
            chequebook.as_slice(),
        );
        let signature = identity.signer().sign_message_sync(&msg).unwrap();

        BzzAddress {
            underlays,
            underlay_bytes,
            overlay,
            signature,
            nonce,
            timestamp,
            chequebook,
        }
    }

    fn local_identity() -> Arc<Identity> {
        Arc::new(Identity::random(init_testnet(), SwarmNodeType::Storer))
    }

    fn remote_identity() -> Arc<Identity> {
        Arc::new(Identity::random(init_testnet(), SwarmNodeType::Storer))
    }

    #[test]
    fn verifier_accepts_well_formed_record() {
        let local = local_identity();
        let remote = remote_identity();
        let now = 1_700_000_000;

        let verifier = DefaultHiveVerifier::with_clock(
            local.spec().network_id(),
            local.overlay_address(),
            move || now,
        );

        let underlays: Vec<libp2p::Multiaddr> =
            vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];
        let addr = signed_bzz_address(&remote, underlays, now);

        let verified = verifier.verify(&addr, GossipSource::Gossip).unwrap();
        assert_eq!(verified.peer.overlay(), &remote.overlay_address());
    }

    #[test]
    fn verifier_rejects_self_overlay() {
        let local = local_identity();
        let now = 1_700_000_000;

        let verifier = DefaultHiveVerifier::with_clock(
            local.spec().network_id(),
            local.overlay_address(),
            move || now,
        );

        let underlays: Vec<libp2p::Multiaddr> =
            vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];
        let addr = signed_bzz_address(&local, underlays, now);
        let err = verifier.verify(&addr, GossipSource::Gossip).unwrap_err();
        assert_eq!(err, HiveRejection::SelfOverlay);
    }

    #[test]
    fn verifier_rejects_future_timestamp_beyond_skew() {
        let local = local_identity();
        let remote = remote_identity();
        let now = 1_700_000_000;

        let verifier = DefaultHiveVerifier::with_clock(
            local.spec().network_id(),
            local.overlay_address(),
            move || now,
        );

        let underlays: Vec<libp2p::Multiaddr> =
            vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];
        let addr = signed_bzz_address(
            &remote,
            underlays,
            now + crate::bzz::MAX_CLOCK_SKEW_SECS + 1,
        );
        let err = verifier.verify(&addr, GossipSource::Gossip).unwrap_err();
        assert_eq!(err, HiveRejection::TimestampOutsideSkewWindow);
    }

    #[test]
    fn verifier_rejects_tampered_overlay() {
        let local = local_identity();
        let remote = remote_identity();
        let now = 1_700_000_000;

        let verifier = DefaultHiveVerifier::with_clock(
            local.spec().network_id(),
            local.overlay_address(),
            move || now,
        );

        let underlays: Vec<libp2p::Multiaddr> =
            vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];
        let mut addr = signed_bzz_address(&remote, underlays, now);
        // Tamper: replace overlay with an unrelated one. Recovery succeeds
        // against the tampered signing data, but overlay re-derivation fails.
        addr.overlay = SwarmAddress::from(B256::from([7u8; 32]));
        let err = verifier.verify(&addr, GossipSource::Gossip).unwrap_err();
        assert_eq!(err, HiveRejection::InvalidSignature);
    }

    #[test]
    fn rejection_labels_stable() {
        assert_eq!(
            <&'static str>::from(HiveRejection::InvalidSignature),
            "invalid_signature"
        );
        assert_eq!(
            <&'static str>::from(HiveRejection::TimestampOutsideSkewWindow),
            "timestamp_outside_skew_window"
        );
        assert_eq!(
            <&'static str>::from(HiveRejection::NetworkIdMismatch),
            "network_id_mismatch"
        );
        assert_eq!(
            <&'static str>::from(HiveRejection::Blocklisted),
            "blocklisted"
        );
        assert_eq!(
            <&'static str>::from(HiveRejection::PrivateMultiaddr),
            "private_multiaddr"
        );
        assert_eq!(
            <&'static str>::from(HiveRejection::Malformed),
            "malformed"
        );
        assert_eq!(
            <&'static str>::from(HiveRejection::SelfOverlay),
            "self_overlay"
        );
    }

    /// A toy blocklist for the with_blocklist test.
    struct StaticBlocklist(SwarmAddress);
    impl BlocklistQuery for StaticBlocklist {
        fn is_blocked(&mut self, overlay: &SwarmAddress) -> bool {
            overlay == &self.0
        }
    }

    #[test]
    fn verifier_rejects_blocklisted_overlay() {
        let local = local_identity();
        let remote = remote_identity();
        let now = 1_700_000_000;

        let blocklist = Arc::new(Mutex::new(StaticBlocklist(remote.overlay_address())));
        let verifier = DefaultHiveVerifier::with_clock(
            local.spec().network_id(),
            local.overlay_address(),
            move || now,
        )
        .with_blocklist(blocklist);

        let underlays: Vec<libp2p::Multiaddr> =
            vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];
        let addr = signed_bzz_address(&remote, underlays, now);
        let err = verifier.verify(&addr, GossipSource::Gossip).unwrap_err();
        assert_eq!(err, HiveRejection::Blocklisted);
    }

    #[test]
    fn verifier_rejects_unsigned_signature() {
        // Construct a BzzAddress carrying a signature that does not recover.
        let local = local_identity();
        let remote = remote_identity();
        let now = 1_700_000_000;

        let verifier = DefaultHiveVerifier::with_clock(
            local.spec().network_id(),
            local.overlay_address(),
            move || now,
        );

        let underlays: Vec<libp2p::Multiaddr> =
            vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];
        let mut addr = signed_bzz_address(&remote, underlays, now);
        // Flip every byte of the underlay to invalidate the signature.
        for b in &mut addr.underlay_bytes {
            *b = b.wrapping_add(1);
        }
        let err = verifier.verify(&addr, GossipSource::Gossip).unwrap_err();
        assert_eq!(err, HiveRejection::InvalidSignature);
    }
}
