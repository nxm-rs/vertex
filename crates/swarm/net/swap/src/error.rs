//! Error types for swap protocol.

vertex_net_codec::protocol_error! {
    /// SWAP protocol errors.
    pub enum SwapError {
        /// Missing required header.
        #[error("missing header: {0}")]
        MissingHeader(&'static str),

        /// Invalid beneficiary address length.
        #[error("invalid beneficiary length: expected 20, got {0}")]
        #[strum(serialize = "invalid_beneficiary")]
        InvalidBeneficiaryLength(usize),

        /// Cheque JSON encoding failure, carrying the typed source error.
        #[error("cheque encode: {0}")]
        #[strum(serialize = "cheque_encode")]
        ChequeEncode(#[source] serde_json::Error),

        /// Cheque JSON decoding failure, carrying the typed source error.
        #[error("cheque decode: {0}")]
        #[strum(serialize = "cheque_decode")]
        ChequeDecode(#[source] serde_json::Error),
    }
}
