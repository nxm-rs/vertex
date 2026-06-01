//! Bee-mainnet wire `BzzAddress` (handshake 15.0.0).
//!
//! The on-wire form (see bee `pkg/p2p/libp2p/internal/handshake/pb/handshake.proto`)
//! carries the peer's underlay set, overlay, signature, nonce, timestamp and an
//! optional chequebook address. The signature is over a domain-separated
//! concatenation defined by bee `pkg/bzz/address.go:138-160` and is recovered
//! using the EIP-191 personal-message prefix (matching bee's
//! `crypto.Signer`/`crypto.Recover`).
//!
//! This module exposes a typed [`BzzAddress`] plus minimal newtype wrappers
//! ([`Nonce`], [`Timestamp`]) so callers never traffic in raw `[u8; N]`. The
//! newtypes mirror the surface intended by the broader typed-primitives effort
//! and are expected to be replaced by a shared crate later without changing
//! the public API here.

use crate::error::SwarmPeerError;
use crate::serde_multiaddr::{deserialize_multiaddrs, serialize_multiaddrs};
use alloy_primitives::{Address, B256, Signature};
use alloy_signer::SignerSync;
use bytes::{Bytes, BytesMut};
use libp2p::Multiaddr;
use nectar_primitives::SwarmAddress;

/// Maximum permitted absolute deviation between a wire `Timestamp` and local
/// wall-clock time (`±6h`). Outside this window the address is rejected.
///
/// This is intentionally wider than bee's handshake-time
/// `MaxClockSkew = 60s` (see `bee/pkg/bzz/timestamp.go`) because it bounds the
/// *acceptable address* itself, not the freshness check performed against a
/// previously-stored record. The narrower per-source freshness rules are
/// applied by higher layers.
pub const MAX_CLOCK_SKEW_SECS: i64 = 6 * 60 * 60;

/// 32-byte handshake nonce (`Address.Nonce` in bee's protobuf).
///
/// Newtype over [`B256`]; participates in the overlay computation
/// `keccak256(eth_address || network_id || nonce)` and is bound into the
/// handshake signature.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(transparent)]
pub struct Nonce(B256);

impl Nonce {
    /// Construct from a 32-byte array.
    #[inline]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(B256::new(bytes))
    }

    /// View as `&[u8; 32]`.
    #[inline]
    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_ref()
    }

    /// View the inner [`B256`].
    #[inline]
    pub const fn as_b256(&self) -> &B256 {
        &self.0
    }
}

impl From<B256> for Nonce {
    #[inline]
    fn from(b: B256) -> Self {
        Self(b)
    }
}

impl From<Nonce> for B256 {
    #[inline]
    fn from(n: Nonce) -> Self {
        n.0
    }
}

impl From<[u8; 32]> for Nonce {
    #[inline]
    fn from(bytes: [u8; 32]) -> Self {
        Self(B256::new(bytes))
    }
}

impl AsRef<[u8]> for Nonce {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

/// Handshake timestamp: seconds since the Unix epoch.
///
/// Bee's wire field (`int64`) carries seconds; values must be strictly
/// positive. This newtype enforces non-negativity at construction time and
/// is serialized as big-endian 8 bytes inside the handshake sign-data.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    /// Construct from a raw `i64`.
    #[inline]
    pub const fn from_secs(secs: i64) -> Self {
        Self(secs)
    }

    /// Inner seconds value.
    #[inline]
    pub const fn as_secs(&self) -> i64 {
        self.0
    }

    /// Convert to big-endian 8 bytes (handshake wire form).
    #[inline]
    pub const fn to_be_bytes(self) -> [u8; 8] {
        self.0.to_be_bytes()
    }

    /// Reject timestamps strictly outside `±MAX_CLOCK_SKEW_SECS` of `now`.
    #[inline]
    pub fn is_within_skew(self, now: Timestamp) -> bool {
        let diff = self.0.saturating_sub(now.0);
        diff.saturating_abs() <= MAX_CLOCK_SKEW_SECS
    }
}

impl From<i64> for Timestamp {
    #[inline]
    fn from(secs: i64) -> Self {
        Self(secs)
    }
}

impl From<Timestamp> for i64 {
    #[inline]
    fn from(ts: Timestamp) -> Self {
        ts.0
    }
}

/// Extended Bee-mainnet wire address used by handshake 15.0.0.
///
/// Compared with the legacy `SwarmPeer` (handshake 14.0.0) this type binds
/// the peer's nonce, a wall-clock timestamp and an optional chequebook
/// address into the handshake signature.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BzzAddress {
    underlay: Vec<Multiaddr>,
    signature: Signature,
    overlay: SwarmAddress,
    nonce: Nonce,
    timestamp: Timestamp,
    chequebook: Option<Address>,
}

impl BzzAddress {
    /// Sign and construct a `BzzAddress` from its components.
    ///
    /// At least one underlay multiaddr is required. The timestamp must be
    /// strictly positive (matches bee's `ErrTimestampInvalid`).
    pub fn sign<S>(
        signer: &S,
        underlay: Vec<Multiaddr>,
        overlay: SwarmAddress,
        network_id: u64,
        nonce: Nonce,
        timestamp: Timestamp,
        chequebook: Option<Address>,
    ) -> Result<Self, SwarmPeerError>
    where
        S: SignerSync + ?Sized,
    {
        if underlay.is_empty() {
            return Err(SwarmPeerError::NoMultiaddrs);
        }
        if timestamp.as_secs() <= 0 {
            return Err(SwarmPeerError::TimestampOutsideSkewWindow);
        }

        let underlay_bytes = serialize_multiaddrs(&underlay);
        let msg = generate_sign_data_v15(
            &underlay_bytes,
            &overlay,
            network_id,
            nonce,
            timestamp,
            chequebook,
        );
        let signature = signer.sign_message_sync(&msg)?;

        Ok(Self {
            underlay,
            signature,
            overlay,
            nonce,
            timestamp,
            chequebook,
        })
    }

    /// Parse and verify a wire `BzzAddress`.
    ///
    /// Recovers the signer from the signature, recomputes the overlay, and
    /// validates it against the claimed value. When `now` is supplied the
    /// timestamp is required to lie within [`MAX_CLOCK_SKEW_SECS`] of it.
    ///
    /// `chequebook_bytes` mirrors bee's protobuf field: empty (`&[]`) means
    /// "no chequebook" and is sign-data-equivalent to 20 zero bytes; any
    /// other length is rejected with [`SwarmPeerError::InvalidChequebook`].
    #[allow(
        clippy::too_many_arguments,
        reason = "every parameter maps to a distinct wire field; bundling would only obscure that"
    )]
    pub fn parse(
        underlay_bytes: &[u8],
        signature: Signature,
        overlay: SwarmAddress,
        nonce: Nonce,
        timestamp: Timestamp,
        network_id: u64,
        chequebook_bytes: &[u8],
        now: Option<Timestamp>,
    ) -> Result<(Self, Address), SwarmPeerError> {
        if timestamp.as_secs() <= 0 {
            return Err(SwarmPeerError::TimestampOutsideSkewWindow);
        }
        if let Some(now) = now
            && !timestamp.is_within_skew(now)
        {
            return Err(SwarmPeerError::TimestampOutsideSkewWindow);
        }

        let chequebook = parse_chequebook(chequebook_bytes)?;

        let underlay = deserialize_multiaddrs(underlay_bytes)?;
        if underlay.is_empty() {
            return Err(SwarmPeerError::NoMultiaddrs);
        }

        let msg = generate_sign_data_v15(
            underlay_bytes,
            &overlay,
            network_id,
            nonce,
            timestamp,
            chequebook,
        );
        let recovered = signature.recover_address_from_msg(msg)?;

        let expected_overlay =
            vertex_swarm_primitives::compute_overlay(&recovered, network_id, nonce.as_b256());
        if expected_overlay != overlay {
            return Err(SwarmPeerError::InvalidOverlay);
        }

        Ok((
            Self {
                underlay,
                signature,
                overlay,
                nonce,
                timestamp,
                chequebook,
            },
            recovered,
        ))
    }

    /// Underlay multiaddrs (must be non-empty for a valid address).
    #[inline]
    pub fn underlay(&self) -> &[Multiaddr] {
        &self.underlay
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

    /// Wire-serialize the underlays (matches bee's `SerializeUnderlays`).
    pub fn serialize_underlay(&self) -> Vec<u8> {
        serialize_multiaddrs(&self.underlay)
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

/// Build the EIP-191-prefixed sign-data for handshake 15.0.0.
///
/// Byte layout (exactly mirrors bee `pkg/bzz/address.go:138-160`):
///
/// ```text
/// "bee-handshake-"                 // 14 bytes, ASCII
/// || underlay_bytes                // variable (bee `SerializeUnderlays`)
/// || overlay                       // 32 bytes
/// || network_id                    // 8 bytes, big-endian
/// || nonce                         // 32 bytes
/// || timestamp                     // 8 bytes, big-endian (uint64 of i64)
/// || chequebook                    // 20 bytes, all-zero when `None`
/// ```
///
/// The EIP-191 personal-message prefix
/// (`"\x19Ethereum Signed Message:\n" || len`) is applied by
/// [`alloy_signer::SignerSync::sign_message_sync`] on sign and by
/// [`alloy_primitives::Signature::recover_address_from_msg`] on recover.
fn generate_sign_data_v15(
    underlay_bytes: &[u8],
    overlay: &SwarmAddress,
    network_id: u64,
    nonce: Nonce,
    timestamp: Timestamp,
    chequebook: Option<Address>,
) -> Bytes {
    const PREFIX: &[u8] = b"bee-handshake-";
    const CHEQUEBOOK_LEN: usize = 20;

    let mut buf = BytesMut::with_capacity(
        PREFIX.len() + underlay_bytes.len() + 32 + 8 + 32 + 8 + CHEQUEBOOK_LEN,
    );
    buf.extend_from_slice(PREFIX);
    buf.extend_from_slice(underlay_bytes);
    buf.extend_from_slice(overlay.as_ref());
    buf.extend_from_slice(&network_id.to_be_bytes());
    buf.extend_from_slice(nonce.as_bytes());
    buf.extend_from_slice(&timestamp.to_be_bytes());
    match chequebook {
        Some(addr) => buf.extend_from_slice(addr.as_slice()),
        None => buf.extend_from_slice(&[0u8; CHEQUEBOOK_LEN]),
    }
    buf.freeze()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_signer_local::PrivateKeySigner;
    use vertex_swarm_primitives::compute_overlay;

    fn now_secs() -> i64 {
        // Tests do not depend on real wall-clock; a fixed value is fine.
        1_700_000_000
    }

    fn signer_with_overlay(network_id: u64, nonce: Nonce) -> (PrivateKeySigner, SwarmAddress) {
        let signer = PrivateKeySigner::random();
        let overlay = compute_overlay(&signer.address(), network_id, nonce.as_b256());
        (signer, overlay)
    }

    #[test]
    fn sign_and_recover_with_chequebook() {
        let network_id = 10u64;
        let nonce = Nonce::from([0x11u8; 32]);
        let timestamp = Timestamp::from_secs(now_secs());
        let chequebook = Some(Address::from([0xAB; 20]));

        let (signer, overlay) = signer_with_overlay(network_id, nonce);
        let underlay: Vec<Multiaddr> = vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];

        let addr = BzzAddress::sign(
            &signer,
            underlay.clone(),
            overlay,
            network_id,
            nonce,
            timestamp,
            chequebook,
        )
        .unwrap();

        let cb_bytes = addr.chequebook().unwrap().as_slice().to_vec();
        let (parsed, recovered) = BzzAddress::parse(
            &addr.serialize_underlay(),
            *addr.signature(),
            *addr.overlay(),
            *addr.nonce(),
            addr.timestamp(),
            network_id,
            &cb_bytes,
            Some(Timestamp::from_secs(now_secs())),
        )
        .unwrap();

        assert_eq!(parsed, addr);
        assert_eq!(recovered, signer.address());
        assert_eq!(parsed.chequebook(), Some(&Address::from([0xAB; 20])));
    }

    #[test]
    fn sign_and_recover_with_no_chequebook() {
        let network_id = 1u64;
        let nonce = Nonce::from([0x22u8; 32]);
        let timestamp = Timestamp::from_secs(now_secs());

        let (signer, overlay) = signer_with_overlay(network_id, nonce);
        let underlay: Vec<Multiaddr> = vec!["/ip4/10.0.0.1/tcp/1633".parse().unwrap()];

        let addr = BzzAddress::sign(
            &signer, underlay, overlay, network_id, nonce, timestamp, None,
        )
        .unwrap();

        // Empty bytes on the wire == None and must verify against all-zero pad.
        let (parsed, recovered) = BzzAddress::parse(
            &addr.serialize_underlay(),
            *addr.signature(),
            *addr.overlay(),
            *addr.nonce(),
            addr.timestamp(),
            network_id,
            &[],
            Some(Timestamp::from_secs(now_secs())),
        )
        .unwrap();

        assert_eq!(parsed.chequebook(), None);
        assert_eq!(recovered, signer.address());
    }

    #[test]
    fn empty_chequebook_bytes_equivalent_to_zero_address_in_sign_data() {
        // A peer that signed with chequebook=None must recover identically when
        // the wire field is sent as empty bytes.
        let network_id = 1u64;
        let nonce = Nonce::from([0x33u8; 32]);
        let timestamp = Timestamp::from_secs(now_secs());

        let (signer, overlay) = signer_with_overlay(network_id, nonce);
        let underlay: Vec<Multiaddr> = vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];

        let addr = BzzAddress::sign(
            &signer, underlay, overlay, network_id, nonce, timestamp, None,
        )
        .unwrap();

        // Manually verify that signing with all-zero chequebook bytes produced
        // the same signature as Option::None.
        let underlay_bytes = addr.serialize_underlay();
        let msg = generate_sign_data_v15(
            &underlay_bytes,
            &overlay,
            network_id,
            nonce,
            timestamp,
            Some(Address::ZERO),
        );
        let recovered = addr.signature().recover_address_from_msg(msg).unwrap();
        assert_eq!(recovered, signer.address());
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
        let network_id = 1u64;
        let nonce = Nonce::from([0u8; 32]);
        let (signer, overlay) = signer_with_overlay(network_id, nonce);
        let underlay: Vec<Multiaddr> = vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];

        let res = BzzAddress::sign(
            &signer,
            underlay,
            overlay,
            network_id,
            nonce,
            Timestamp::from_secs(0),
            None,
        );
        assert!(matches!(
            res,
            Err(SwarmPeerError::TimestampOutsideSkewWindow)
        ));
    }

    #[test]
    fn rejects_empty_underlay_on_sign() {
        let network_id = 1u64;
        let nonce = Nonce::from([0u8; 32]);
        let (signer, overlay) = signer_with_overlay(network_id, nonce);
        let res = BzzAddress::sign(
            &signer,
            vec![],
            overlay,
            network_id,
            nonce,
            Timestamp::from_secs(now_secs()),
            None,
        );
        assert!(matches!(res, Err(SwarmPeerError::NoMultiaddrs)));
    }

    #[test]
    fn timestamp_skew_boundaries() {
        let network_id = 1u64;
        let nonce = Nonce::from([0u8; 32]);
        let now = now_secs();
        let (signer, overlay) = signer_with_overlay(network_id, nonce);
        let underlay: Vec<Multiaddr> = vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];

        // Helper: sign + parse against `now`, return whether parsing succeeded.
        let try_parse = |signed_at: i64| -> Result<(), SwarmPeerError> {
            let ts = Timestamp::from_secs(signed_at);
            let addr = BzzAddress::sign(
                &signer,
                underlay.clone(),
                overlay,
                network_id,
                nonce,
                ts,
                None,
            )?;
            BzzAddress::parse(
                &addr.serialize_underlay(),
                *addr.signature(),
                *addr.overlay(),
                *addr.nonce(),
                addr.timestamp(),
                network_id,
                &[],
                Some(Timestamp::from_secs(now)),
            )
            .map(|_| ())
        };

        // Exactly +6h: accepted (inclusive boundary).
        try_parse(now + MAX_CLOCK_SKEW_SECS).expect("+6h boundary accepted");
        // Exactly -6h: accepted.
        try_parse(now - MAX_CLOCK_SKEW_SECS).expect("-6h boundary accepted");
        // +6h + 1s: rejected.
        let res = try_parse(now + MAX_CLOCK_SKEW_SECS + 1);
        assert!(matches!(
            res,
            Err(SwarmPeerError::TimestampOutsideSkewWindow)
        ));
        // -6h - 1s: rejected.
        let res = try_parse(now - MAX_CLOCK_SKEW_SECS - 1);
        assert!(matches!(
            res,
            Err(SwarmPeerError::TimestampOutsideSkewWindow)
        ));
    }

    #[test]
    fn skipping_skew_check_when_now_is_none() {
        // Caller can opt out of skew enforcement entirely.
        let network_id = 1u64;
        let nonce = Nonce::from([0u8; 32]);
        let (signer, overlay) = signer_with_overlay(network_id, nonce);
        let underlay: Vec<Multiaddr> = vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];

        let ts = Timestamp::from_secs(now_secs() + 10 * MAX_CLOCK_SKEW_SECS);
        let addr =
            BzzAddress::sign(&signer, underlay, overlay, network_id, nonce, ts, None).unwrap();

        let (_, recovered) = BzzAddress::parse(
            &addr.serialize_underlay(),
            *addr.signature(),
            *addr.overlay(),
            *addr.nonce(),
            addr.timestamp(),
            network_id,
            &[],
            None,
        )
        .unwrap();
        assert_eq!(recovered, signer.address());
    }

    #[test]
    fn invalid_overlay_rejected() {
        let network_id = 1u64;
        let nonce = Nonce::from([0u8; 32]);
        let (signer, _overlay) = signer_with_overlay(network_id, nonce);
        let bogus_overlay = SwarmAddress::from(B256::repeat_byte(0xFF));
        let underlay: Vec<Multiaddr> = vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];

        // Sign with bogus overlay — signature is valid for the chosen bytes
        // but recovered overlay won't match.
        let addr = BzzAddress::sign(
            &signer,
            underlay,
            bogus_overlay,
            network_id,
            nonce,
            Timestamp::from_secs(now_secs()),
            None,
        )
        .unwrap();
        let res = BzzAddress::parse(
            &addr.serialize_underlay(),
            *addr.signature(),
            *addr.overlay(),
            *addr.nonce(),
            addr.timestamp(),
            network_id,
            &[],
            Some(Timestamp::from_secs(now_secs())),
        );
        assert!(matches!(res, Err(SwarmPeerError::InvalidOverlay)));
    }
}
