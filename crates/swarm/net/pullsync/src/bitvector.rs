//! MSB-first selection bitvector for the pullsync `Want` reply.
//!
//! Bit `i` (the high bit of byte `i / 8`, counting from the most significant)
//! is set exactly when the requester wants `chunks[i]` from the preceding
//! `Offer`. The byte length is `ceil(len / 8)`; trailing pad bits in the final
//! byte are zero and unused.

/// A fixed-length set of selection bits, packed MSB-first one bit per offered
/// chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitVector {
    bytes: Vec<u8>,
    len: usize,
}

impl BitVector {
    /// An all-clear vector sized for `len` chunks.
    #[must_use]
    pub fn new(len: usize) -> Self {
        Self {
            bytes: vec![0u8; len.div_ceil(8)],
            len,
        }
    }

    /// Wrap raw wire bytes, sizing the selection at one bit per byte
    /// (`8 * bytes.len()`).
    ///
    /// The `Want` frame carries no explicit chunk count, so the byte length is
    /// the only length signal; the trailing pad bits of the final byte are
    /// preserved but address no chunk in the offer.
    #[must_use]
    pub fn from_wire_bytes(bytes: Vec<u8>) -> Self {
        let len = bytes.len() * 8;
        Self { bytes, len }
    }

    /// Wrap raw wire bytes as a vector selecting over `len` chunks.
    ///
    /// `bytes` must be exactly `ceil(len / 8)` long, the only length a
    /// conformant peer sends for an offer of `len` chunks.
    pub fn from_bytes(bytes: Vec<u8>, len: usize) -> Result<Self, BitVectorError> {
        let expected = len.div_ceil(8);
        if bytes.len() != expected {
            return Err(BitVectorError {
                expected,
                got: bytes.len(),
            });
        }
        Ok(Self { bytes, len })
    }

    /// The number of chunks this vector selects over.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether this vector selects over no chunks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether `chunks[i]` is wanted. `false` for any `i >= len`.
    #[must_use]
    pub fn get(&self, i: usize) -> bool {
        if i >= self.len {
            return false;
        }
        self.bytes
            .get(i / 8)
            .is_some_and(|byte| byte & (0x80 >> (i % 8)) != 0)
    }

    /// Mark `chunks[i]` as wanted. No-op for `i >= len`.
    pub fn set(&mut self, i: usize) {
        if i < self.len
            && let Some(byte) = self.bytes.get_mut(i / 8)
        {
            *byte |= 0x80 >> (i % 8);
        }
    }

    /// The count of set bits, i.e. how many deliveries the offer answers with.
    #[must_use]
    pub fn count_ones(&self) -> usize {
        (0..self.len).filter(|&i| self.get(i)).count()
    }

    /// The packed wire bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consume into the packed wire bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// The `Want` bitvector byte length did not match the offer it answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("bitvector length mismatch: expected {expected} bytes, got {got}")]
pub struct BitVectorError {
    /// Bytes required for the offer (`ceil(len / 8)`).
    pub expected: usize,
    /// Bytes actually present on the wire.
    pub got: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_i_is_msb_first_within_each_byte() {
        let mut bv = BitVector::new(16);
        bv.set(0);
        bv.set(7);
        bv.set(8);
        bv.set(15);
        // Byte 0: bits 0 and 7 -> 0b1000_0001 = 0x81.
        // Byte 1: bits 8 and 15 -> 0b1000_0001 = 0x81.
        assert_eq!(bv.as_bytes(), &[0x81, 0x81]);
        assert!(bv.get(0));
        assert!(bv.get(7));
        assert!(bv.get(8));
        assert!(bv.get(15));
        assert!(!bv.get(1));
        assert!(!bv.get(9));
    }

    #[test]
    fn single_high_bit_is_0x80() {
        let mut bv = BitVector::new(8);
        bv.set(0);
        assert_eq!(bv.as_bytes(), &[0x80]);
    }

    #[test]
    fn byte_length_is_ceil_div_eight() {
        assert_eq!(BitVector::new(0).as_bytes().len(), 0);
        assert_eq!(BitVector::new(1).as_bytes().len(), 1);
        assert_eq!(BitVector::new(8).as_bytes().len(), 1);
        assert_eq!(BitVector::new(9).as_bytes().len(), 2);
        assert_eq!(BitVector::new(250).as_bytes().len(), 32);
    }

    #[test]
    fn count_ones_counts_selected_chunks() {
        let mut bv = BitVector::new(10);
        bv.set(1);
        bv.set(3);
        bv.set(9);
        assert_eq!(bv.count_ones(), 3);
    }

    #[test]
    fn out_of_range_index_is_clear_and_set_is_noop() {
        let mut bv = BitVector::new(3);
        bv.set(5);
        assert!(!bv.get(5));
        assert_eq!(bv.count_ones(), 0);
    }

    #[test]
    fn from_bytes_rejects_wrong_length() {
        assert!(BitVector::from_bytes(vec![0u8; 2], 16).is_ok());
        let err = BitVector::from_bytes(vec![0u8; 1], 16).expect_err("too short");
        assert_eq!(err.expected, 2);
        assert_eq!(err.got, 1);
    }

    #[test]
    fn round_trips_through_wire_bytes() {
        let mut bv = BitVector::new(20);
        bv.set(2);
        bv.set(17);
        let len = bv.len();
        let restored = BitVector::from_bytes(bv.clone().into_bytes(), len).expect("valid bytes");
        assert_eq!(restored, bv);
    }
}
