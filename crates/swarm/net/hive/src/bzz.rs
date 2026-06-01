//! Local minimal `BzzAddress` for hive 2.0.0.
//!
//! This type mirrors bee's `pkg/bzz.Address` on the wire and is sufficient for
//! hive 2.0.0 peer-record verification. It will be replaced by the canonical
//! `BzzAddress` introduced by Unit 2 of the indexed-snacking-crown plan once
//! that lands; the field layout is intentionally identical.
//!
//! ### Sign data layout (bee 15.0.0+)
//!
//! ```text
//! "bee-handshake-"        // 14 bytes
//! || underlay_bytes       // wire-encoded multiaddrs (length-prefixed entries)
//! || overlay              // 32 bytes
//! || network_id           // 8 bytes, big-endian
//! || nonce                // 32 bytes
//! || timestamp            // 8 bytes, big-endian (unix seconds, as u64 cast)
//! || chequebook_address   // 0 or 20 bytes (zeros == "no chequebook")
//! ```
//!
//! Reference: `bee/pkg/bzz/address.go` `generateSignData` (lines 138-160).
//! The EIP-191 personal-sign prefix is applied by
//! [`alloy_primitives::Signature::recover_address_from_msg`].
//!
//! ### Timestamp skew window
//!
//! Per `bee/pkg/bzz/timestamp.go`:
//! - timestamp must be strictly positive,
//! - timestamp must not be more than `MAX_CLOCK_SKEW_SECS` seconds in the future.
//!
//! Source-specific staleness checks (gossip vs handshake) are the responsibility
//! of higher layers that compare against an existing stored record.

use alloy_primitives::{Address, B256, Signature};
use bytes::{Bytes, BytesMut};
use libp2p::Multiaddr;
use vertex_swarm_peer::{SwarmAddress, SwarmPeer, deserialize_multiaddrs};

/// Required nonce length in bytes (matches bee `bzz.NonceLength`).
pub const NONCE_LENGTH: usize = 32;
/// Ethereum address length in bytes.
pub const CHEQUEBOOK_ADDRESS_LENGTH: usize = 20;

/// Maximum permitted clock skew when validating a received timestamp (60s,
/// matching bee `bzz.MaxClockSkew`).
pub const MAX_CLOCK_SKEW_SECS: i64 = 60;

/// Signing prefix shared by bee handshake/hive records.
const SIGN_PREFIX: &[u8] = b"bee-handshake-";

/// Wire form of a single hive peer record. Mirrors bee's `BzzAddress`.
///
/// This is the minimal Unit 6 local shim — Unit 2 will replace it with the
/// canonical type from the peers crate.
#[derive(Debug, Clone)]
pub struct BzzAddress {
    /// Underlay multiaddrs (parsed).
    pub underlays: Vec<Multiaddr>,
    /// Raw underlay bytes as received on the wire — kept so callers can
    /// re-verify the signature without re-serializing (bee's underlay
    /// serializer is not idempotent across implementations).
    pub underlay_bytes: Vec<u8>,
    /// Overlay (Swarm) address.
    pub overlay: SwarmAddress,
    /// Signature over `generate_sign_data(...)`.
    pub signature: Signature,
    /// 32-byte nonce mixed into overlay derivation.
    pub nonce: B256,
    /// Unix-seconds timestamp at which the record was minted by its owner.
    pub timestamp: i64,
    /// Owner's chequebook address. `Address::ZERO` is treated as "no chequebook".
    pub chequebook: Address,
}

/// Why a wire-encoded peer record was rejected before reaching the routing
/// table.
///
/// Every variant doubles as a stable metric label via [`strum::IntoStaticStr`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum BzzAddressError {
    /// Nonce is not exactly [`NONCE_LENGTH`] bytes.
    NonceLength,
    /// Overlay is not 32 bytes.
    OverlayLength,
    /// Chequebook field is neither empty nor 20 bytes.
    ChequebookLength,
    /// Signature failed to parse.
    SignatureFormat,
    /// Timestamp must be strictly positive.
    TimestampInvalid,
    /// No usable underlays after deserialisation.
    NoUnderlays,
    /// Underlay bytes failed to deserialise.
    UnderlayDecode,
}

impl core::fmt::Display for BzzAddressError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(<&'static str>::from(*self))
    }
}

impl std::error::Error for BzzAddressError {}

impl BzzAddress {
    /// Construct a `BzzAddress` from wire fields, validating only the cheap
    /// structural invariants (lengths and timestamp sign).
    ///
    /// Does NOT verify the signature — call [`Self::verify_signature`] for
    /// that. We keep the two steps separate so the caller can decide whether
    /// to run signature recovery on the blocking executor.
    pub fn from_wire(
        multiaddrs_bytes: Vec<u8>,
        signature_bytes: &[u8],
        overlay_bytes: &[u8],
        nonce_bytes: &[u8],
        timestamp: i64,
        chequebook_bytes: &[u8],
    ) -> Result<Self, BzzAddressError> {
        if nonce_bytes.len() != NONCE_LENGTH {
            return Err(BzzAddressError::NonceLength);
        }
        if !(chequebook_bytes.is_empty() || chequebook_bytes.len() == CHEQUEBOOK_ADDRESS_LENGTH) {
            return Err(BzzAddressError::ChequebookLength);
        }
        if timestamp <= 0 {
            return Err(BzzAddressError::TimestampInvalid);
        }

        let overlay = B256::try_from(overlay_bytes).map_err(|_| BzzAddressError::OverlayLength)?;
        let nonce = B256::try_from(nonce_bytes).map_err(|_| BzzAddressError::NonceLength)?;
        let signature =
            Signature::try_from(signature_bytes).map_err(|_| BzzAddressError::SignatureFormat)?;
        let chequebook = if chequebook_bytes.is_empty() {
            Address::ZERO
        } else {
            Address::from_slice(chequebook_bytes)
        };

        let underlays = deserialize_multiaddrs(&multiaddrs_bytes)
            .map_err(|_| BzzAddressError::UnderlayDecode)?;
        if underlays.is_empty() {
            return Err(BzzAddressError::NoUnderlays);
        }

        Ok(Self {
            underlays,
            underlay_bytes: multiaddrs_bytes,
            overlay: SwarmAddress::from(overlay),
            signature,
            nonce,
            timestamp,
            chequebook,
        })
    }

    /// Verify the EIP-191 signature and return the recovered signer address.
    ///
    /// The caller is responsible for cross-checking the recovered signer
    /// against the overlay (via `compute_overlay`) when overlay-derivation
    /// verification is desired.
    pub fn verify_signature(&self, network_id: u64) -> Result<Address, BzzAddressError> {
        let msg = generate_sign_data(
            &self.underlay_bytes,
            self.overlay.as_ref(),
            network_id,
            self.nonce.as_slice(),
            self.timestamp,
            self.chequebook.as_slice(),
        );
        self.signature
            .recover_address_from_msg(msg)
            .map_err(|_| BzzAddressError::SignatureFormat)
    }

    /// Convert into a [`SwarmPeer`] after the signature has been verified.
    ///
    /// `ethereum_address` is the recovered signer from [`Self::verify_signature`].
    pub fn into_swarm_peer(self, ethereum_address: Address) -> SwarmPeer {
        SwarmPeer::from_validated(
            self.underlays,
            self.signature,
            B256::from(self.overlay),
            self.nonce,
            ethereum_address,
        )
    }
}

/// Verify the timestamp is within the allowed skew window relative to `now`.
///
/// `now` is unix-seconds. Strictly future-dated records by more than
/// [`MAX_CLOCK_SKEW_SECS`] are rejected. The lower bound (zero/negative) is
/// enforced in [`BzzAddress::from_wire`].
pub fn check_timestamp_skew(timestamp: i64, now: i64) -> bool {
    if timestamp <= 0 {
        return false;
    }
    timestamp <= now.saturating_add(MAX_CLOCK_SKEW_SECS)
}

/// Build the bee `bzz` sign-data byte sequence.
///
/// Layout documented at the module level. Returns a [`Bytes`] so callers can
/// hand it to `Signature::recover_address_from_msg` without re-copying.
pub fn generate_sign_data(
    underlay: &[u8],
    overlay: &[u8],
    network_id: u64,
    nonce: &[u8],
    timestamp: i64,
    chequebook: &[u8],
) -> Bytes {
    let mut buf = BytesMut::with_capacity(
        SIGN_PREFIX.len() + underlay.len() + overlay.len() + 8 + nonce.len() + 8 + chequebook.len(),
    );
    buf.extend_from_slice(SIGN_PREFIX);
    buf.extend_from_slice(underlay);
    buf.extend_from_slice(overlay);
    buf.extend_from_slice(&network_id.to_be_bytes());
    buf.extend_from_slice(nonce);
    buf.extend_from_slice(&(timestamp as u64).to_be_bytes());
    buf.extend_from_slice(chequebook);
    buf.freeze()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn timestamp_rejects_negative_and_zero() {
        assert!(!check_timestamp_skew(0, 1_000));
        assert!(!check_timestamp_skew(-1, 1_000));
    }

    #[test]
    fn timestamp_accepts_within_skew_window() {
        let now = 1_700_000_000;
        assert!(check_timestamp_skew(now, now));
        assert!(check_timestamp_skew(now - 3600, now));
        assert!(check_timestamp_skew(now + MAX_CLOCK_SKEW_SECS, now));
    }

    #[test]
    fn timestamp_rejects_too_far_future() {
        let now = 1_700_000_000;
        assert!(!check_timestamp_skew(now + MAX_CLOCK_SKEW_SECS + 1, now));
    }

    #[test]
    fn from_wire_rejects_bad_nonce_length() {
        let err = BzzAddress::from_wire(vec![], &[0u8; 65], &[0u8; 32], &[0u8; 16], 1, &[])
            .unwrap_err();
        assert!(matches!(err, BzzAddressError::NonceLength));
    }

    #[test]
    fn from_wire_rejects_bad_chequebook_length() {
        let err =
            BzzAddress::from_wire(vec![], &[0u8; 65], &[0u8; 32], &[0u8; 32], 1, &[0u8; 7])
                .unwrap_err();
        assert!(matches!(err, BzzAddressError::ChequebookLength));
    }

    #[test]
    fn from_wire_rejects_nonpositive_timestamp() {
        let err =
            BzzAddress::from_wire(vec![], &[0u8; 65], &[0u8; 32], &[0u8; 32], 0, &[]).unwrap_err();
        assert!(matches!(err, BzzAddressError::TimestampInvalid));
    }

    #[test]
    fn error_label_stable() {
        assert_eq!(
            <&'static str>::from(BzzAddressError::NonceLength),
            "nonce_length"
        );
        assert_eq!(
            <&'static str>::from(BzzAddressError::TimestampInvalid),
            "timestamp_invalid"
        );
    }
}
