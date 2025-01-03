use alloy_primitives::FixedBytes;

pub mod bmt;
pub mod distance;
pub mod proximity;

/// Address represents an address in Swarm metric space of Node and Chunk addresses.
pub type Address = FixedBytes<32>;

pub const STAMP_INDEX_SIZE: usize = 8;
pub const STAMP_TIMESTAMP_SIZE: usize = 8;
pub const SPAN_SIZE: usize = 8;
pub const SEGMENT_SIZE: usize = 32;
pub const BRANCHES: usize = 128;
pub const ENCRYPTED_BRANCHES: usize = BRANCHES / 2;
pub const BMT_BRANCHES: usize = 128;
pub const CHUNK_SIZE: usize = SEGMENT_SIZE * BRANCHES;
pub const HASH_SIZE: usize = 32;
pub const MAX_PO: usize = 31;
pub const EXTENDED_PO: usize = MAX_PO + 5;
pub const MAX_BINS: usize = MAX_PO + 1;
pub const CHUNK_WITH_SPAN_SIZE: usize = CHUNK_SIZE + SPAN_SIZE;
pub const SOC_SIGNATURE_SIZE: usize = 65;
pub const SOC_MIN_CHUNK_SIZE: usize = HASH_SIZE + SOC_SIGNATURE_SIZE + SPAN_SIZE;
pub const SOC_MAX_CHUNK_SIZE: usize = SOC_MIN_CHUNK_SIZE + CHUNK_SIZE;

#[cfg(test)]
mod tests {
    use alloy_primitives::hex::FromHex;

    use super::*;

    #[test]
    fn parse_text() {
        let h: &str = "ee1e4ffa36b01b6b5d5d8173931c289f448422c6e015854842cd3349fbbc06d0";
        let t = Address::from_hex(h).unwrap();
        assert_ne!(t, Address::ZERO);

        assert!(t.eq(&t));
    }
}
