//! Fuzz-facing surface behind the `arbitrary` feature: an owned wire-record
//! holder, adversarial generators, and the re-exports the fuzz targets need.
//! Dev-only; never enabled by a shipped artefact cone.

use alloy_primitives::U256;
use arbitrary::Unstructured;

pub use alloy_primitives::{Address, Signature};
pub use core::time::Duration;
pub use libp2p::Multiaddr;
pub use nectar_primitives::NetworkId;

pub use crate::{
    ARBITRARY_NETWORK_ID, MAX_CLOCK_SKEW, MAX_MULTIADDRS_PER_PEER, MIN_UPDATE_INTERVAL, Nonce,
    OverlayAddress, SwarmPeer, SwarmPeerError, SwarmPeerWire, Timestamp, TimestampRejection,
    arbitrary_multiaddr, arbitrary_multiaddr_with_peer_id, check_timestamp, deserialize_multiaddrs,
    serialize_multiaddrs,
};

/// Owned wire-record fields; [`Self::as_wire`] borrows them as the
/// [`SwarmPeer::parse`] input.
#[derive(Debug, Clone)]
pub struct WireRecord {
    /// Serialized multiaddr block.
    pub multiaddrs_bytes: Vec<u8>,
    /// 65-byte secp256k1 signature.
    pub signature: Signature,
    /// Claimed overlay address.
    pub overlay: OverlayAddress,
    /// Handshake nonce.
    pub nonce: Nonce,
    /// Wall-clock timestamp in seconds.
    pub timestamp: Timestamp,
    /// Empty or 20-byte chequebook field.
    pub chequebook_bytes: Vec<u8>,
}

impl WireRecord {
    /// Borrow as the parse input.
    #[must_use]
    pub fn as_wire(&self) -> SwarmPeerWire<'_> {
        SwarmPeerWire {
            multiaddrs_bytes: &self.multiaddrs_bytes,
            signature: self.signature,
            overlay: self.overlay,
            nonce: self.nonce,
            timestamp: self.timestamp,
            chequebook_bytes: &self.chequebook_bytes,
        }
    }

    /// The wire fields of a signed record.
    #[must_use]
    pub fn from_peer(peer: &SwarmPeer) -> Self {
        Self {
            multiaddrs_bytes: peer.serialize_multiaddrs(),
            signature: *peer.signature(),
            overlay: *peer.overlay(),
            nonce: *peer.nonce(),
            timestamp: peer.timestamp(),
            chequebook_bytes: peer
                .chequebook()
                .map(|a| a.as_slice().to_vec())
                .unwrap_or_default(),
        }
    }
}

/// A multiaddrs bytes field: usually a well-formed block (possibly over the
/// count cap), otherwise raw hostile bytes for the hand-rolled list decoder.
pub fn arbitrary_multiaddrs_bytes(u: &mut Unstructured<'_>) -> arbitrary::Result<Vec<u8>> {
    if u.arbitrary()? {
        let count = u.int_in_range(0..=MAX_MULTIADDRS_PER_PEER + 4)?;
        let addrs = (0..count)
            .map(|_| arbitrary_multiaddr(u))
            .collect::<arbitrary::Result<Vec<_>>>()?;
        Ok(serialize_multiaddrs(&addrs))
    } else {
        u.arbitrary()
    }
}

/// True for the one multiaddr-block shape the single-address encoding cannot
/// round-trip: a lone multiaddr whose wire bytes open with the list prefix
/// (e.g. `/webrtc`, bytes `0x99 0x02`), which the decoder then reads back as a
/// list marker. This prefix collision is inherent to the wire format, so a
/// re-encode round-trip oracle must exclude it.
#[must_use]
pub fn reencodes_ambiguously(addrs: &[Multiaddr]) -> bool {
    matches!(
        addrs,
        [only] if only.to_vec().first() == Some(&crate::serde_multiaddr::MULTIADDR_LIST_PREFIX)
    )
}

/// Flip one drawn bit in `bytes`; a no-op on an empty slice.
fn flip_bit(u: &mut Unstructured<'_>, bytes: &mut [u8]) -> arbitrary::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    let index = u.choose_index(bytes.len())?;
    let mask = 1u8 << u.int_in_range(0..=7u8)?;
    if let Some(byte) = bytes.get_mut(index) {
        *byte ^= mask;
    }
    Ok(())
}

/// An adversarial wire record: either hostile field content throughout, or a
/// really signed record under [`ARBITRARY_NETWORK_ID`] with at most one
/// tampered field, so both the recovery success path and every rejection arm
/// of the parse path stay reachable.
pub fn arbitrary_wire_record(u: &mut Unstructured<'_>) -> arbitrary::Result<WireRecord> {
    if u.arbitrary()? {
        let peer = SwarmPeer::arbitrary_signed(u, ARBITRARY_NETWORK_ID)?;
        let mut record = WireRecord::from_peer(&peer);
        match u.int_in_range(0..=4u8)? {
            // Untampered: recovery and overlay validation succeed.
            0 => {}
            1 => {
                let mut bytes = record.signature.as_bytes();
                flip_bit(u, &mut bytes)?;
                if let Ok(signature) = Signature::from_raw(&bytes) {
                    record.signature = signature;
                }
            }
            2 => {
                let mut overlay = <[u8; 32]>::from(record.overlay);
                flip_bit(u, &mut overlay)?;
                record.overlay = OverlayAddress::new(overlay);
            }
            3 => record.timestamp = Timestamp::from_seconds(u.arbitrary()?),
            _ => record.chequebook_bytes = u.arbitrary()?,
        }
        return Ok(record);
    }
    Ok(WireRecord {
        multiaddrs_bytes: arbitrary_multiaddrs_bytes(u)?,
        signature: Signature::new(
            U256::from_be_bytes(u.arbitrary::<[u8; 32]>()?),
            U256::from_be_bytes(u.arbitrary::<[u8; 32]>()?),
            u.arbitrary()?,
        ),
        overlay: OverlayAddress::new(u.arbitrary()?),
        nonce: Nonce::new(u.arbitrary()?),
        timestamp: Timestamp::from_seconds(u.arbitrary()?),
        chequebook_bytes: u.arbitrary()?,
    })
}
