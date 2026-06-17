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
        /// Carries the message string rather than the source error: chunk
        /// reconstruction now returns nectar's `StampError`, which `InvalidStamp`
        /// already claims via `#[from]`, so this variant cannot also derive a
        /// `From<StampError>`. The call site maps the error to its string.
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

        /// A custody receipt's signature did not recover to a signer overlay.
        /// An all-zero (structural failure) signature, or any signature that
        /// fails public-key recovery, lands here. Such a receipt is rejected at
        /// the decode boundary and never reaches a domain consumer.
        #[error("custody receipt signature did not recover to a signer overlay")]
        MalformedReceiptSignature,
    }
}

impl PushsyncError {
    /// True when the error is a malformed-chunk signal: the delivered bytes
    /// failed address or stamp reconstruction, or the address itself was
    /// unparseable. These are attributable to the sending peer and warrant an
    /// adverse score, distinct from a transport or negotiation failure. Receipt
    /// decoding errors are excluded since they describe a storer's reply, not a
    /// chunk a peer pushed at us.
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
        // A bad nonce length describes a storer reply, not a chunk a peer
        // pushed at us, so it must not be scored as invalid data.
        assert!(!PushsyncError::InvalidNonceLength(3).is_invalid_chunk());
        assert!(!PushsyncError::ConnectionClosed.is_invalid_chunk());
    }
}
