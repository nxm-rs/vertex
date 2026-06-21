//! Bee-mainnet wire `SwarmPeer` (handshake 15.0.0).
//!
//! The on-wire form (see bee `pkg/p2p/libp2p/internal/handshake/pb/handshake.proto`)
//! carries the peer's multiaddrs, overlay, signature, nonce, timestamp and an
//! optional chequebook address. The signature is over a domain-separated
//! concatenation defined by bee `pkg/bzz/address.go:138-160` and is recovered
//! using the EIP-191 personal-message prefix (matching bee's
//! `crypto.Signer`/`crypto.Recover`).
//!
//! The canonical sign-data byte layout lives in
//! [`nectar_primitives::signing::sign_data`]; this module is the libp2p-aware
//! wrapper that owns the `Vec<Multiaddr>` (nectar deliberately stays
//! libp2p-free) and binds it to the typed [`Nonce`] and [`Timestamp`] also
//! re-exported from nectar.

use crate::error::SwarmPeerError;
use crate::serde_multiaddr::{deserialize_multiaddrs, serialize_multiaddrs};
use alloy_primitives::{Address, Signature};
use libp2p::Multiaddr;
use nectar_primitives::signing::sign_data;
use nectar_primitives::{NetworkId, SwarmAddress, compute_overlay};
pub use nectar_primitives::{Nonce, Timestamp};
use std::time::Duration;
use vertex_net_local::{AddressScope, IpCapability, classify_multiaddr};
use vertex_swarm_primitives::OverlaySigner;

/// Borrowed view of the on-wire `SwarmPeer` fields, used as input to
/// [`SwarmPeer::parse`].
///
/// Mirrors the protobuf layout of the wire `SwarmPeer` message
/// (multiaddrs, signature, overlay, nonce, timestamp, chequebook_address).
/// Borrows the variable-length byte fields to avoid forcing the caller
/// into ownership transfer.
#[derive(Clone, Copy, Debug)]
pub struct SwarmPeerWire<'a> {
    /// Bee-serialized multiaddrs (`SerializeUnderlays` in bee).
    pub multiaddrs_bytes: &'a [u8],
    /// 65-byte secp256k1 signature over the EIP-191 sign-data.
    pub signature: Signature,
    /// Claimed overlay address; validated against the recovered signer.
    pub overlay: SwarmAddress,
    /// Handshake nonce.
    pub nonce: Nonce,
    /// Wall-clock timestamp in seconds since the Unix epoch.
    pub timestamp: Timestamp,
    /// Empty for "no chequebook"; otherwise exactly 20 bytes.
    pub chequebook_bytes: &'a [u8],
}

/// Bee-mainnet wire address (handshake 15.0.0).
///
/// Binds the peer's multiaddrs, overlay, nonce, wall-clock
/// timestamp and an optional chequebook into a single EIP-191 handshake
/// signature using the canonical sign-data layout in
/// [`nectar_primitives::signing::sign_data`].
///
/// The `ethereum_address` is the EIP-191 signer recovered from the
/// signature at construction time (sign or parse) and cached. Callers
/// never need to redo recovery.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SwarmPeer {
    multiaddrs: Vec<Multiaddr>,
    signature: Signature,
    overlay: SwarmAddress,
    nonce: Nonce,
    timestamp: Timestamp,
    chequebook: Option<Address>,
    ethereum_address: Address,
}

impl SwarmPeer {
    /// Sign and construct a `SwarmPeer` from the node identity plus the
    /// per-handshake fields.
    ///
    /// The overlay, `network_id` and `nonce` are read from `identity`, and the
    /// overlay is derived the same way [`parse`](Self::parse) recovers it, so a
    /// record this node signs cannot bind an overlay inconsistent with the key
    /// that signed it. At least one multiaddr is required; the timestamp must
    /// be strictly positive (matches bee's `ErrTimestampInvalid`).
    pub fn sign(
        identity: &impl OverlaySigner,
        multiaddrs: Vec<Multiaddr>,
        timestamp: Timestamp,
        chequebook: Option<Address>,
    ) -> Result<Self, SwarmPeerError> {
        if multiaddrs.is_empty() {
            return Err(SwarmPeerError::NoMultiaddrs);
        }
        if timestamp.get() <= 0 {
            return Err(SwarmPeerError::InvalidTimestamp);
        }
        // Normalise the all-zero chequebook to `None` to prevent on-wire
        // malleability: bee's `generateSignData` produces byte-identical
        // sign-data for `None` and `Some(Address::ZERO)`, so a peer that
        // signed with one would still verify with the other. By collapsing
        // both into `None` here we keep `chequebook().is_some()` a faithful
        // signal of "peer actually advertised a chequebook".
        let chequebook = chequebook.filter(|a| !a.is_zero());

        let network_id = identity.network_id();
        let nonce = identity.nonce();
        let overlay = identity.overlay();

        let multiaddrs_bytes = serialize_multiaddrs(&multiaddrs);
        let msg = sign_data(
            &multiaddrs_bytes,
            &overlay,
            network_id,
            &nonce,
            timestamp,
            chequebook.as_ref(),
        );
        let signature = identity.sign_message_sync(&msg)?;
        // Cache the signer's ethereum address rather than re-recover on every
        // access.
        let ethereum_address = identity.address();

        Ok(Self {
            multiaddrs,
            signature,
            overlay,
            nonce,
            timestamp,
            chequebook,
            ethereum_address,
        })
    }

    /// Parse and verify a wire `SwarmPeer`.
    ///
    /// Recovers the signer from the signature, recomputes the overlay, and
    /// validates it against the claimed value. When `skew` is supplied the
    /// timestamp is required to lie within the given tolerance of the
    /// provided "now". Pass `None` to opt out of the skew check entirely.
    ///
    /// The `chequebook_bytes` field of [`SwarmPeerWire`] mirrors bee's
    /// protobuf field: empty (`&[]`) means "no chequebook" and is
    /// sign-data-equivalent to 20 zero bytes; any other length is rejected
    /// with [`SwarmPeerError::InvalidChequebook`].
    pub fn parse(
        wire: SwarmPeerWire<'_>,
        network_id: NetworkId,
        skew: Option<(Timestamp, Duration)>,
    ) -> Result<Self, SwarmPeerError> {
        let SwarmPeerWire {
            multiaddrs_bytes,
            signature,
            overlay,
            nonce,
            timestamp,
            chequebook_bytes,
        } = wire;

        // `<= 0` is a structural protocol violation (bee `ErrTimestampInvalid`),
        // distinct from a clock-skew failure; it is rejected unconditionally
        // even when `skew == None`.
        if timestamp.get() <= 0 {
            return Err(SwarmPeerError::InvalidTimestamp);
        }
        if let Some((now, tolerance)) = skew
            && timestamp.skew_check(now, tolerance).is_err()
        {
            return Err(SwarmPeerError::TimestampOutsideSkewWindow);
        }

        // Normalise the all-zero chequebook to `None` (see `sign` above for
        // the malleability rationale).
        let chequebook = parse_chequebook(chequebook_bytes)?.filter(|a| !a.is_zero());

        let multiaddrs = deserialize_multiaddrs(multiaddrs_bytes)?;
        if multiaddrs.is_empty() {
            return Err(SwarmPeerError::NoMultiaddrs);
        }

        let msg = sign_data(
            multiaddrs_bytes,
            &overlay,
            network_id,
            &nonce,
            timestamp,
            chequebook.as_ref(),
        );
        let ethereum_address = signature.recover_address_from_msg(msg)?;

        let expected_overlay = compute_overlay(&ethereum_address, network_id, &nonce);
        if expected_overlay != overlay {
            return Err(SwarmPeerError::InvalidOverlay);
        }

        Ok(Self {
            multiaddrs,
            signature,
            overlay,
            nonce,
            timestamp,
            chequebook,
            ethereum_address,
        })
    }

    /// Multiaddrs (must be non-empty for a valid address).
    #[inline]
    pub fn multiaddrs(&self) -> &[Multiaddr] {
        &self.multiaddrs
    }

    /// First multiaddrs multiaddr (always present, never panics for a valid
    /// `SwarmPeer`: `sign`/`parse` reject empty multiaddrs sets).
    #[inline]
    pub fn multiaddr(&self) -> Option<&Multiaddr> {
        self.multiaddrs.first()
    }

    /// Handshake signature.
    #[inline]
    pub fn signature(&self) -> &Signature {
        &self.signature
    }

    /// Overlay address.
    #[inline]
    pub fn overlay(&self) -> &SwarmAddress {
        &self.overlay
    }

    /// Handshake nonce.
    #[inline]
    pub fn nonce(&self) -> &Nonce {
        &self.nonce
    }

    /// Handshake timestamp.
    #[inline]
    pub fn timestamp(&self) -> Timestamp {
        self.timestamp
    }

    /// Optional chequebook address.
    #[inline]
    pub fn chequebook(&self) -> Option<&Address> {
        self.chequebook.as_ref()
    }

    /// Ethereum address that signed this record.
    ///
    /// Cached at construction (either from the signer in [`Self::sign`] or
    /// recovered from the signature in [`Self::parse`]); never re-derived
    /// on access.
    #[inline]
    pub fn ethereum_address(&self) -> &Address {
        &self.ethereum_address
    }

    /// Wire-serialize the multiaddrs (matches bee's `SerializeUnderlays`).
    pub fn serialize_multiaddrs(&self) -> Vec<u8> {
        serialize_multiaddrs(&self.multiaddrs)
    }

    /// Test-only constructor that bypasses signature verification.
    ///
    /// Gated behind `test-utils`/`#[cfg(test)]` so it cannot be reached from
    /// production code paths. The caller is responsible for any invariants
    /// (signature/overlay/recovery consistency); fixtures use it to
    /// fabricate deterministic peer records for routing-only tests where
    /// the signature is never verified.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn from_parts(
        multiaddrs: Vec<Multiaddr>,
        signature: Signature,
        overlay: SwarmAddress,
        nonce: Nonce,
        timestamp: Timestamp,
        chequebook: Option<Address>,
        ethereum_address: Address,
    ) -> Self {
        Self {
            multiaddrs,
            signature,
            overlay,
            nonce,
            timestamp,
            chequebook,
            ethereum_address,
        }
    }

    /// IP capability across the peer's multiaddrs multiaddrs (v4 / v6 / dual).
    pub fn ip_capability(&self) -> IpCapability {
        IpCapability::from_addrs(&self.multiaddrs)
    }

    /// Filter multiaddrs multiaddrs by address scope.
    pub fn addrs_by_scope(&self, scope: AddressScope) -> Vec<Multiaddr> {
        self.multiaddrs
            .iter()
            .filter(|addr| classify_multiaddr(addr) == Some(scope))
            .cloned()
            .collect()
    }

    /// Whether the peer advertises any address of the given scope.
    pub fn has_scope(&self, scope: AddressScope) -> bool {
        self.multiaddrs
            .iter()
            .any(|addr| classify_multiaddr(addr) == Some(scope))
    }

    /// Highest address scope present (Public > LinkLocal > Private > Loopback).
    pub fn max_scope(&self) -> Option<AddressScope> {
        self.multiaddrs
            .iter()
            .filter_map(classify_multiaddr)
            .max_by_key(scope_rank)
    }
}

fn scope_rank(scope: &AddressScope) -> u8 {
    match scope {
        AddressScope::Public => 3,
        AddressScope::LinkLocal => 2,
        AddressScope::Private => 1,
        AddressScope::Loopback => 0,
    }
}

/// Parse the optional chequebook field from its wire bytes.
///
/// Bee's `ParseAddress` accepts either an empty byte slice (no chequebook)
/// or exactly `common.AddressLength` (= 20) bytes. Any other length is a
/// protocol violation and is rejected here.
fn parse_chequebook(bytes: &[u8]) -> Result<Option<Address>, SwarmPeerError> {
    match bytes.len() {
        0 => Ok(None),
        20 => {
            let mut buf = [0u8; 20];
            buf.copy_from_slice(bytes);
            Ok(Some(Address::from(buf)))
        }
        _ => Err(SwarmPeerError::InvalidChequebook),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "panics are the expected failure mode for tests"
)]
mod tests {
    use super::*;
    use alloy_signer::SignerSync;
    use alloy_signer_local::PrivateKeySigner;
    use nectar_primitives::{MAINNET, SwarmSpec};
    use std::sync::Arc;
    use vertex_swarm_identity::Identity;
    use vertex_swarm_primitives::SwarmNodeType;
    use vertex_swarm_spec::SpecBuilder;

    fn now_secs() -> i64 {
        // Tests do not depend on real wall-clock; a fixed value is fine.
        1_700_000_000
    }

    /// Canonical 6h skew tolerance from nectar's default spec.
    fn skew_tolerance() -> Duration {
        MAINNET.clock_skew_tolerance()
    }

    fn skew_tolerance_secs() -> i64 {
        skew_tolerance().as_secs() as i64
    }

    /// A persistent identity with the given network id and nonce, so the
    /// derived overlay is deterministic for the test.
    fn test_identity(network_id: NetworkId, nonce: Nonce) -> Identity {
        let spec = Arc::new(SpecBuilder::testnet().network_id(network_id.get()).build());
        Identity::new(
            PrivateKeySigner::random(),
            nonce,
            spec,
            SwarmNodeType::Storer,
        )
    }

    /// Build a borrowed wire view from a `SwarmPeer` plus the serialized
    /// multiaddrs bytes and the chequebook bytes both held by the caller.
    fn wire<'a>(
        addr: &SwarmPeer,
        multiaddrs_bytes: &'a [u8],
        chequebook_bytes: &'a [u8],
    ) -> SwarmPeerWire<'a> {
        SwarmPeerWire {
            multiaddrs_bytes,
            signature: *addr.signature(),
            overlay: *addr.overlay(),
            nonce: *addr.nonce(),
            timestamp: addr.timestamp(),
            chequebook_bytes,
        }
    }

    #[test]
    fn sign_and_recover_with_chequebook() {
        let network_id = NetworkId::new(10);
        let nonce = Nonce::from([0x11u8; 32]);
        let timestamp = Timestamp::from_seconds(now_secs());
        let chequebook = Some(Address::from([0xAB; 20]));

        let identity = test_identity(network_id, nonce);
        let multiaddrs: Vec<Multiaddr> = vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];

        let addr = SwarmPeer::sign(&identity, multiaddrs, timestamp, chequebook).unwrap();

        let multiaddrs_bytes = addr.serialize_multiaddrs();
        let cb_bytes = addr.chequebook().unwrap().as_slice().to_vec();
        let parsed = SwarmPeer::parse(
            wire(&addr, &multiaddrs_bytes, &cb_bytes),
            network_id,
            Some((Timestamp::from_seconds(now_secs()), skew_tolerance())),
        )
        .unwrap();

        assert_eq!(parsed, addr);
        assert_eq!(*parsed.ethereum_address(), identity.address());
        assert_eq!(parsed.chequebook(), Some(&Address::from([0xAB; 20])));
    }

    #[test]
    fn sign_and_recover_with_no_chequebook() {
        let network_id = NetworkId::new(1);
        let nonce = Nonce::from([0x22u8; 32]);
        let timestamp = Timestamp::from_seconds(now_secs());

        let identity = test_identity(network_id, nonce);
        let multiaddrs: Vec<Multiaddr> = vec!["/ip4/10.0.0.1/tcp/1633".parse().unwrap()];

        let addr = SwarmPeer::sign(&identity, multiaddrs, timestamp, None).unwrap();

        // Empty bytes on the wire == None and must verify against all-zero pad.
        let multiaddrs_bytes = addr.serialize_multiaddrs();
        let parsed = SwarmPeer::parse(
            wire(&addr, &multiaddrs_bytes, &[]),
            network_id,
            Some((Timestamp::from_seconds(now_secs()), skew_tolerance())),
        )
        .unwrap();

        assert_eq!(parsed.chequebook(), None);
        assert_eq!(*parsed.ethereum_address(), identity.address());
    }

    #[test]
    fn empty_chequebook_bytes_equivalent_to_zero_address_in_sign_data() {
        // A peer that signed with chequebook=None must recover identically when
        // the wire field is sent as empty bytes.
        let network_id = NetworkId::new(1);
        let nonce = Nonce::from([0x33u8; 32]);
        let timestamp = Timestamp::from_seconds(now_secs());

        let identity = test_identity(network_id, nonce);
        let multiaddrs: Vec<Multiaddr> = vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];

        let addr = SwarmPeer::sign(&identity, multiaddrs, timestamp, None).unwrap();

        // Manually verify that signing with all-zero chequebook bytes produced
        // the same signature as Option::None.
        let multiaddrs_bytes = addr.serialize_multiaddrs();
        let zero_cb = Address::ZERO;
        let msg = sign_data(
            &multiaddrs_bytes,
            &identity.overlay(),
            network_id,
            &nonce,
            timestamp,
            Some(&zero_cb),
        );
        let recovered = addr.signature().recover_address_from_msg(msg).unwrap();
        assert_eq!(recovered, identity.address());
    }

    #[test]
    fn rejects_wrong_chequebook_length() {
        let res = parse_chequebook(&[0u8; 19]);
        assert!(matches!(res, Err(SwarmPeerError::InvalidChequebook)));
        let res = parse_chequebook(&[0u8; 21]);
        assert!(matches!(res, Err(SwarmPeerError::InvalidChequebook)));
    }

    #[test]
    fn rejects_non_positive_timestamp_on_sign() {
        let network_id = NetworkId::new(1);
        let nonce = Nonce::from([0u8; 32]);
        let identity = test_identity(network_id, nonce);
        let multiaddrs: Vec<Multiaddr> = vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];

        // Non-positive timestamp is a structural protocol violation,
        // distinct from a clock-skew failure: it returns `InvalidTimestamp`
        // (bee `ErrTimestampInvalid`), not `TimestampOutsideSkewWindow`.
        let res = SwarmPeer::sign(&identity, multiaddrs, Timestamp::from_seconds(0), None);
        assert!(matches!(res, Err(SwarmPeerError::InvalidTimestamp)));
    }

    #[test]
    fn parse_rejects_non_positive_timestamp_even_without_skew() {
        // Documents the contract distinction between `InvalidTimestamp`
        // (structural, always rejected) and `TimestampOutsideSkewWindow`
        // (only fires when `skew == Some(_)`). With `skew = None` a
        // timestamp of 0 must still be rejected as `InvalidTimestamp`.
        let network_id = NetworkId::new(1);
        let nonce = Nonce::from([0u8; 32]);
        let identity = test_identity(network_id, nonce);
        let multiaddrs: Vec<Multiaddr> = vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];
        // Sign with a valid timestamp so we have a real wire form, then
        // fabricate a wire view with `timestamp = 0` for the parse-side test.
        let addr = SwarmPeer::sign(
            &identity,
            multiaddrs,
            Timestamp::from_seconds(now_secs()),
            None,
        )
        .unwrap();
        let multiaddrs_bytes = addr.serialize_multiaddrs();
        let mut bogus_wire = wire(&addr, &multiaddrs_bytes, &[]);
        bogus_wire.timestamp = Timestamp::from_seconds(0);
        let res = SwarmPeer::parse(bogus_wire, network_id, None);
        assert!(matches!(res, Err(SwarmPeerError::InvalidTimestamp)));
    }

    #[test]
    fn parse_normalises_zero_chequebook_to_none() {
        // Wire malleability defence: bee's `generateSignData` produces
        // byte-identical bytes for `None` and `Some(Address::ZERO)`. An
        // attacker on the path could rewrite the wire field from empty to
        // 20 zero bytes (or vice versa) and the signature would still
        // verify. Parsing must collapse both into `None` so downstream
        // `chequebook().is_some()` is not malleable.
        let network_id = NetworkId::new(1);
        let nonce = Nonce::from([0x33u8; 32]);
        let identity = test_identity(network_id, nonce);
        let multiaddrs: Vec<Multiaddr> = vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];

        // Sign with `Some(Address::ZERO)` and confirm `chequebook()` is None
        // both immediately after signing and after a wire roundtrip.
        let addr = SwarmPeer::sign(
            &identity,
            multiaddrs,
            Timestamp::from_seconds(now_secs()),
            Some(Address::ZERO),
        )
        .unwrap();
        assert_eq!(
            addr.chequebook(),
            None,
            "sign should normalise zero -> None"
        );

        let multiaddrs_bytes = addr.serialize_multiaddrs();
        // Force the wire chequebook to the 20-zero-bytes form an attacker
        // could substitute; parse must still produce `chequebook = None`.
        let zero_bytes = [0u8; 20];
        let parsed = SwarmPeer::parse(
            wire(&addr, &multiaddrs_bytes, &zero_bytes),
            network_id,
            None,
        )
        .expect("parse with zero-bytes chequebook should succeed");
        assert_eq!(parsed.chequebook(), None);
    }

    #[test]
    fn rejects_empty_multiaddrs_on_sign() {
        let network_id = NetworkId::new(1);
        let nonce = Nonce::from([0u8; 32]);
        let identity = test_identity(network_id, nonce);
        let res = SwarmPeer::sign(&identity, vec![], Timestamp::from_seconds(now_secs()), None);
        assert!(matches!(res, Err(SwarmPeerError::NoMultiaddrs)));
    }

    #[test]
    fn timestamp_skew_boundaries() {
        let network_id = NetworkId::new(1);
        let nonce = Nonce::from([0u8; 32]);
        let now = now_secs();
        let tolerance = skew_tolerance();
        let tolerance_secs = skew_tolerance_secs();
        let identity = test_identity(network_id, nonce);
        let multiaddrs: Vec<Multiaddr> = vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];

        // Helper: sign + parse against `now`, return whether parsing succeeded.
        let try_parse = |signed_at: i64| -> Result<(), SwarmPeerError> {
            let ts = Timestamp::from_seconds(signed_at);
            let addr = SwarmPeer::sign(&identity, multiaddrs.clone(), ts, None)?;
            let multiaddrs_bytes = addr.serialize_multiaddrs();
            SwarmPeer::parse(
                wire(&addr, &multiaddrs_bytes, &[]),
                network_id,
                Some((Timestamp::from_seconds(now), tolerance)),
            )
            .map(|_| ())
        };

        // Exactly +6h: accepted (inclusive boundary).
        try_parse(now + tolerance_secs).expect("+6h boundary accepted");
        // Exactly -6h: accepted.
        try_parse(now - tolerance_secs).expect("-6h boundary accepted");
        // +6h + 1s: rejected.
        let res = try_parse(now + tolerance_secs + 1);
        assert!(matches!(
            res,
            Err(SwarmPeerError::TimestampOutsideSkewWindow)
        ));
        // -6h - 1s: rejected.
        let res = try_parse(now - tolerance_secs - 1);
        assert!(matches!(
            res,
            Err(SwarmPeerError::TimestampOutsideSkewWindow)
        ));
    }

    #[test]
    fn skipping_skew_check_when_now_is_none() {
        // Caller can opt out of skew enforcement entirely.
        let network_id = NetworkId::new(1);
        let nonce = Nonce::from([0u8; 32]);
        let identity = test_identity(network_id, nonce);
        let multiaddrs: Vec<Multiaddr> = vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];

        let ts = Timestamp::from_seconds(now_secs() + 10 * skew_tolerance_secs());
        let addr = SwarmPeer::sign(&identity, multiaddrs, ts, None).unwrap();

        let multiaddrs_bytes = addr.serialize_multiaddrs();
        let parsed =
            SwarmPeer::parse(wire(&addr, &multiaddrs_bytes, &[]), network_id, None).unwrap();
        assert_eq!(*parsed.ethereum_address(), identity.address());
    }

    #[test]
    fn invalid_overlay_rejected() {
        let network_id = NetworkId::new(1);
        let nonce = Nonce::from([0u8; 32]);
        let signer = PrivateKeySigner::random();
        let bogus_overlay = SwarmAddress::new([0xFF; 32]);
        let multiaddrs: Vec<Multiaddr> = vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];
        let timestamp = Timestamp::from_seconds(now_secs());

        // `sign` now derives the overlay from the identity, so a mismatch can
        // only be fabricated on the wire: sign over the bogus overlay (the
        // signature is valid for those bytes) and claim it, but parse recovers
        // the signer's real overlay, which will not match.
        let multiaddrs_bytes = serialize_multiaddrs(&multiaddrs);
        let msg = sign_data(
            &multiaddrs_bytes,
            &bogus_overlay,
            network_id,
            &nonce,
            timestamp,
            None,
        );
        let signature = signer.sign_message_sync(&msg).unwrap();
        let bogus_wire = SwarmPeerWire {
            multiaddrs_bytes: &multiaddrs_bytes,
            signature,
            overlay: bogus_overlay,
            nonce,
            timestamp,
            chequebook_bytes: &[],
        };
        let res = SwarmPeer::parse(
            bogus_wire,
            network_id,
            Some((Timestamp::from_seconds(now_secs()), skew_tolerance())),
        );
        assert!(matches!(res, Err(SwarmPeerError::InvalidOverlay)));
    }
}
