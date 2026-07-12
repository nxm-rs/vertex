//! Bee-compatible multiaddr serialization.
//!
//! Handles single addresses (raw bytes) and multiple addresses (0x99 prefix + varint lengths).
//!
//! BEE-COMPAT(SWIP-148): the `0x99`-prefixed multi-multiaddr block is a custom byte
//! layout jammed into a protobuf `bytes` field. Required for v1 wire
//! conformance; a SWIP candidate should replace it with `repeated bytes`. See
//! `docs/agents/swarm-protocol.md` ("Wire-compat shims and SWIPs").

use crate::error::MultiAddrError;
use libp2p::Multiaddr;
use std::io::{Cursor, Read};

/// Magic byte prefix for lists of multiple multiaddrs.
/// A conformant single multiaddr starts with an address protocol whose varint
/// never begins with 0x99; the one collision (a lone /webrtc component, whose
/// code varint-encodes as 0x99 0x02) is handled by the ambiguity guard the
/// round-trip oracle consults.
///
/// BEE-COMPAT(SWIP-148): see module docs.
pub(crate) const MULTIADDR_LIST_PREFIX: u8 = 0x99;

/// Maximum multiaddrs accepted per peer record. A gossiped or handshaked record
/// carrying more is rejected whole, so a peer cannot inflate resident table
/// memory or amplify dial fan-out by flooding fabricated addresses. Matches the
/// reference wire cap, so a conformant peer (which advertises a handful) is
/// never rejected. Outbound record producers must bound their advertised set to
/// this cap or their own record is rejected by every conformant peer.
pub const MAX_MULTIADDRS_PER_PEER: usize = 20;

/// Serialize multiaddrs to bytes.
///
/// - Single address: raw bytes (backward compatible)
/// - Zero or multiple: 0x99 prefix + varint-length-prefixed entries
pub fn serialize_multiaddrs(addrs: &[Multiaddr]) -> Vec<u8> {
    // Single address: return raw bytes for backward compatibility
    if let [single] = addrs {
        return single.to_vec();
    }

    let mut buf = Vec::new();
    buf.push(MULTIADDR_LIST_PREFIX);

    for addr in addrs {
        let addr_bytes = addr.to_vec();
        buf.extend(encode_uvarint(addr_bytes.len() as u64));
        buf.extend(addr_bytes);
    }

    buf
}

/// Deserialize bytes to multiaddrs.
///
/// - Empty: returns empty vec (inbound-only peer)
/// - 0x99 prefix: list format
/// - Otherwise: single legacy multiaddr
pub fn deserialize_multiaddrs(data: &[u8]) -> Result<Vec<Multiaddr>, MultiAddrError> {
    match data.split_first() {
        None => Ok(Vec::new()),
        Some((&MULTIADDR_LIST_PREFIX, rest)) => deserialize_list(rest),
        Some(_) => {
            let addr = Multiaddr::try_from(data.to_vec())?;
            Ok(vec![addr])
        }
    }
}

fn deserialize_list(data: &[u8]) -> Result<Vec<Multiaddr>, MultiAddrError> {
    let mut addrs = Vec::new();
    let mut cursor = Cursor::new(data);

    while (cursor.position() as usize) < data.len() {
        // Reject an over-full record before decoding the next entry, so a flood
        // of addresses never allocates past the cap.
        if addrs.len() >= MAX_MULTIADDRS_PER_PEER {
            return Err(MultiAddrError::CountExceeded {
                max: MAX_MULTIADDRS_PER_PEER,
            });
        }

        let addr_len = decode_uvarint(&mut cursor)?;

        let remaining = data.len() - cursor.position() as usize;
        if (addr_len as usize) > remaining {
            return Err(MultiAddrError::InconsistentLength {
                expected: addr_len,
                actual: remaining,
            });
        }

        let mut addr_bytes = vec![0u8; addr_len as usize];
        cursor.read_exact(&mut addr_bytes)?;

        let addr = Multiaddr::try_from(addr_bytes)?;
        addrs.push(addr);
    }

    Ok(addrs)
}

fn encode_uvarint(mut value: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if value == 0 {
            break;
        }
    }
    buf
}

fn decode_uvarint(cursor: &mut Cursor<&[u8]>) -> Result<u64, std::io::Error> {
    let mut result: u64 = 0;
    let mut shift = 0;

    loop {
        let mut byte = [0u8; 1];
        cursor.read_exact(&mut byte)?;
        let b = byte[0];

        result |= ((b & 0x7F) as u64) << shift;
        if b & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift >= 64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "varint too long",
            ));
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn single_addr_roundtrip() {
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();
        let serialized = serialize_multiaddrs(std::slice::from_ref(&addr));
        let deserialized = deserialize_multiaddrs(&serialized).unwrap();

        assert_eq!(deserialized.len(), 1);
        assert_eq!(deserialized[0], addr);
    }

    #[test]
    fn multiple_addrs_roundtrip() {
        let addr1: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();
        let addr2: Multiaddr = "/ip4/192.168.1.1/tcp/5678".parse().unwrap();

        let serialized = serialize_multiaddrs(&[addr1.clone(), addr2.clone()]);
        assert_eq!(serialized[0], MULTIADDR_LIST_PREFIX);

        let deserialized = deserialize_multiaddrs(&serialized).unwrap();
        assert_eq!(deserialized.len(), 2);
        assert_eq!(deserialized[0], addr1);
        assert_eq!(deserialized[1], addr2);
    }

    #[test]
    fn empty_addrs_roundtrip() {
        let serialized = serialize_multiaddrs(&[]);
        assert_eq!(serialized[0], MULTIADDR_LIST_PREFIX);

        let deserialized = deserialize_multiaddrs(&serialized).unwrap();
        assert!(deserialized.is_empty());
    }

    fn n_addrs(n: usize) -> Vec<Multiaddr> {
        (0..n)
            .map(|i| format!("/ip4/127.0.0.1/tcp/{}", 1000 + i).parse().unwrap())
            .collect()
    }

    #[test]
    fn a_record_at_the_cap_deserializes() {
        let addrs = n_addrs(MAX_MULTIADDRS_PER_PEER);
        let serialized = serialize_multiaddrs(&addrs);

        let deserialized = deserialize_multiaddrs(&serialized).unwrap();
        assert_eq!(deserialized.len(), MAX_MULTIADDRS_PER_PEER);
        assert_eq!(deserialized, addrs);
    }

    #[test]
    fn a_record_over_the_cap_is_rejected() {
        let serialized = serialize_multiaddrs(&n_addrs(MAX_MULTIADDRS_PER_PEER + 1));

        let err = deserialize_multiaddrs(&serialized).unwrap_err();
        assert!(
            matches!(err, MultiAddrError::CountExceeded { max } if max == MAX_MULTIADDRS_PER_PEER),
            "over-full record must reject with CountExceeded, got {err:?}"
        );
    }

    /// Replays the committed fuzz seeds through the exact multiaddr-block
    /// decode the `swarm_peer_parse` fuzz target drives on raw bytes, so the
    /// stable test gate proves the seeds stay panic-free without the fuzzer.
    #[test]
    fn seed_replay_swarm_peer_parse() {
        let seed_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../fuzz/seeds/swarm_peer_parse");
        let mut replayed = 0usize;
        for entry in std::fs::read_dir(&seed_dir)
            .unwrap_or_else(|e| panic!("seed dir {} must exist: {e}", seed_dir.display()))
        {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let data = std::fs::read(&path).unwrap();

            let decoded = deserialize_multiaddrs(&data);

            if name.starts_with("valid-") || name.starts_with("edge-") {
                let addrs = decoded.unwrap_or_else(|e| panic!("seed {name} must decode: {e}"));
                assert!(addrs.len() <= MAX_MULTIADDRS_PER_PEER, "seed {name}");
                // Canonical re-encode round-trips, save for the lone-addr
                // shapes the single-addr encoding cannot carry (see the
                // helper).
                if !crate::fuzz::reencodes_ambiguously(&addrs) {
                    let again = deserialize_multiaddrs(&serialize_multiaddrs(&addrs))
                        .unwrap_or_else(|e| panic!("seed {name} must re-decode: {e}"));
                    assert_eq!(again, addrs);
                }
            } else if name.starts_with("invalid-") {
                assert!(decoded.is_err(), "seed {name} must stay an Err");
            } else {
                panic!("seed {name} matches no known prefix");
            }
            replayed += 1;
        }
        assert!(
            replayed >= 7,
            "expected at least the 7 curated seeds, found {replayed}"
        );
    }
}
