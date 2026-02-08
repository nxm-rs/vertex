//! Bee-compatible multiaddr serialization.
//!
//! Handles single addresses (raw bytes) and multiple addresses (0x99 prefix + varint lengths).

use crate::error::MultiAddrError;
use libp2p::Multiaddr;
use std::io::{Cursor, Read};

/// Magic byte prefix for lists of multiple multiaddrs.
/// Chosen because 0x99 is not a valid multiaddr protocol code.
const MULTIADDR_LIST_PREFIX: u8 = 0x99;

/// Serialize multiaddrs to bytes.
///
/// - Single address: raw bytes (backward compatible)
/// - Zero or multiple: 0x99 prefix + varint-length-prefixed entries
pub fn serialize_multiaddrs(addrs: &[Multiaddr]) -> Vec<u8> {
    if addrs.len() == 1 {
        return addrs[0].to_vec();
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
    if data.is_empty() {
        return Ok(Vec::new());
    }

    if data[0] == MULTIADDR_LIST_PREFIX {
        return deserialize_list(&data[1..]);
    }

    let addr = Multiaddr::try_from(data.to_vec())?;
    Ok(vec![addr])
}

fn deserialize_list(data: &[u8]) -> Result<Vec<Multiaddr>, MultiAddrError> {
    let mut addrs = Vec::new();
    let mut cursor = Cursor::new(data);

    while (cursor.position() as usize) < data.len() {
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
}
