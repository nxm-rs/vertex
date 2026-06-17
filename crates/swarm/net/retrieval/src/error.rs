//! Error types for retrieval protocol.

vertex_net_codec::protocol_error! {
    /// Retrieval protocol errors.
    pub enum RetrievalError {
        /// Invalid chunk address length.
        #[error("invalid chunk address length: expected 32, got {0}")]
        InvalidAddressLength(usize),

        /// Invalid chunk address encoding.
        #[error("invalid chunk address: {0}")]
        InvalidAddress(#[from] nectar_primitives::PrimitivesError),

        /// Malformed postage stamp in the delivery.
        #[error("invalid stamp: {0}")]
        InvalidStamp(#[from] nectar_postage::StampError),

        /// Chunk bytes did not match the requested address.
        ///
        /// Carries the message string rather than the source error: chunk
        /// reconstruction now goes through nectar's `AnyChunk::from_wire_bytes`,
        /// which returns `PrimitivesError` — already claimed by `InvalidAddress`
        /// via `#[from]` — so this variant cannot also derive a
        /// `From<PrimitivesError>`. The call site maps the error to its string.
        #[error("invalid chunk: {0}")]
        InvalidChunk(String),
    }
}

impl RetrievalError {
    /// True when the error is a malformed-chunk signal: the delivered bytes
    /// failed address or stamp reconstruction, or the address itself was
    /// unparseable. These are attributable to the sending peer and warrant an
    /// adverse score, distinct from a transport or negotiation failure.
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
        assert!(RetrievalError::InvalidAddressLength(7).is_invalid_chunk());
    }

    #[test]
    fn transport_errors_are_not_invalid_chunk() {
        assert!(!RetrievalError::ConnectionClosed.is_invalid_chunk());
    }
}
