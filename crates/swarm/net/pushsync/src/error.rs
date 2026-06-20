//! Error types for pushsync protocol.

vertex_net_codec::protocol_error! {
    /// Pushsync protocol errors.
    pub enum PushsyncError {
        /// Invalid chunk address length.
        #[error("invalid chunk address length: expected 32, got {0}")]
        InvalidAddressLength(usize),

        /// Invalid chunk address encoding.
        #[error("invalid chunk address: {0}")]
        InvalidAddress(#[from] nectar_primitives::PrimitivesError),

        /// Malformed postage stamp in the delivery.
        #[error("invalid stamp: {0}")]
        InvalidStamp(#[from] nectar_postage::StampError),

        /// Chunk bytes did not match the delivery's address.
        ///
        /// Carries a string rather than a source error because the underlying
        /// `StampError` is already claimed by `InvalidStamp` via `#[from]`.
        #[error("invalid chunk: {0}")]
        InvalidChunk(String),

        /// Malformed receipt signature.
        #[error("invalid signature: {0}")]
        InvalidSignature(#[from] alloy_primitives::SignatureError),

        /// Receipt nonce was not exactly 32 bytes.
        #[error("invalid nonce length: expected 32, got {0}")]
        InvalidNonceLength(usize),

        /// Storage radius is outside the valid bin range.
        #[error("invalid storage radius: {0}")]
        InvalidStorageRadius(u32),

        /// Custody receipt signature failed public-key recovery (including the
        /// all-zero structural-failure signature). Rejected at the decode boundary.
        #[error("custody receipt signature did not recover to a signer overlay")]
        MalformedReceiptSignature,
    }
}

impl PushsyncError {
    /// True for malformed-chunk signals attributable to the sending peer
    /// (address or stamp reconstruction failed). Excludes receipt-decode errors,
    /// which describe a storer's reply rather than a chunk pushed at us, and so
    /// must not feed an adverse score.
    #[must_use]
    pub fn is_invalid_chunk(&self) -> bool {
        matches!(
            self,
            Self::InvalidChunk(_)
                | Self::InvalidStamp(_)
                | Self::InvalidAddress(_)
                | Self::InvalidAddressLength(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_chunk_signals_are_invalid_chunk() {
        assert!(PushsyncError::InvalidAddressLength(7).is_invalid_chunk());
    }

    #[test]
    fn receipt_decode_errors_are_not_invalid_chunk() {
        assert!(!PushsyncError::InvalidNonceLength(3).is_invalid_chunk());
        assert!(!PushsyncError::ConnectionClosed.is_invalid_chunk());
    }
}
