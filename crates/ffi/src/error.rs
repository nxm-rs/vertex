//! Error type for the FFI surface.

use flutter_rust_bridge::frb;

/// Failure modes an embedding host can observe across the FFI boundary.
///
/// Each variant maps a stage of the embedded client lifecycle (build, upload,
/// download) to a flat, language-neutral error. The wrapped strings are already
/// formatted so the host does not need to understand any vertex-internal error
/// type.
#[frb(non_opaque)]
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum FfiError {
    /// The supplied private key was not a 32-byte value.
    #[error("invalid private key: expected 32 bytes, got {len}")]
    InvalidPrivateKey {
        /// Length of the rejected key in bytes.
        len: usize,
    },

    /// The supplied chunk address was not a 32-byte value.
    #[error("invalid chunk address: expected 32 bytes, got {len}")]
    InvalidAddress {
        /// Length of the rejected address in bytes.
        len: usize,
    },

    /// The postage stamp bytes did not decode to a valid stamp.
    #[error("invalid postage stamp: {reason}")]
    InvalidStamp {
        /// Why the stamp failed to decode.
        reason: String,
    },

    /// The chunk bytes did not reconstruct to the supplied address.
    #[error("chunk does not match the supplied address: {reason}")]
    ChunkMismatch {
        /// Why reconstruction failed.
        reason: String,
    },

    /// Building the embedded client node failed.
    #[error("failed to build client: {reason}")]
    Build {
        /// Why the build failed.
        reason: String,
    },

    /// A network upload failed.
    #[error("upload failed: {reason}")]
    Upload {
        /// Why the upload failed.
        reason: String,
    },

    /// A network download failed.
    #[error("download failed: {reason}")]
    Download {
        /// Why the download failed.
        reason: String,
    },

    /// Installing the logging subscriber failed.
    #[error("logging setup failed: {reason}")]
    Logging {
        /// Why logging setup failed (an unparseable filter or a subscriber that
        /// could not be installed).
        reason: String,
    },

    /// Logging was already initialized for this process.
    ///
    /// A process has exactly one global tracing subscriber, so the first
    /// `init_logging` call wins and a later call is rejected without disturbing
    /// the installed subscriber.
    #[error("logging is already initialized for this process")]
    LoggingAlreadyInitialized,
}

/// Convenience alias for results crossing the FFI boundary.
pub type FfiResult<T> = Result<T, FfiError>;
