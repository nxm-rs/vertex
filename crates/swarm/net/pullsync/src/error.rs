//! Error types for pullsync protocol.

vertex_net_codec::protocol_error! {
    /// Pullsync protocol errors.
    pub enum PullsyncError {
        /// Bin index on a `Get` was outside `0..=MAX_PO`.
        #[error("invalid bin: {0}")]
        InvalidBin(i32),

        /// A descriptor field was not exactly 32 bytes. `field` names the
        /// offending field, `len` is the length received.
        #[error("invalid {field} length: expected 32, got {len}")]
        InvalidFieldLength {
            field: &'static str,
            len: usize,
        },

        /// Invalid chunk address encoding.
        #[error("invalid chunk address: {0}")]
        InvalidAddress(#[from] nectar_primitives::PrimitivesError),

        /// Malformed postage stamp in a delivery.
        #[error("invalid stamp: {0}")]
        InvalidStamp(#[from] nectar_postage::StampError),

        /// Chunk bytes did not match the delivery's address.
        ///
        /// Carries a string rather than a source error because the underlying
        /// `PrimitivesError` is already claimed by `InvalidAddress` via `#[from]`.
        #[error("invalid chunk: {0}")]
        InvalidChunk(String),
    }
}

impl PullsyncError {
    /// True for malformed chunk or descriptor signals attributable to the
    /// sending peer, which warrant an adverse score; false for transport or
    /// negotiation failures.
    #[must_use]
    pub fn is_invalid_chunk(&self) -> bool {
        matches!(
            self,
            Self::InvalidChunk(_)
                | Self::InvalidStamp(_)
                | Self::InvalidAddress(_)
                | Self::InvalidFieldLength { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_descriptor_signals_are_invalid_chunk() {
        assert!(
            PullsyncError::InvalidFieldLength {
                field: "address",
                len: 7
            }
            .is_invalid_chunk()
        );
    }

    #[test]
    fn transport_errors_are_not_invalid_chunk() {
        assert!(!PullsyncError::ConnectionClosed.is_invalid_chunk());
        assert!(!PullsyncError::InvalidBin(99).is_invalid_chunk());
    }
}
