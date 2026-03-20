//! Error types for node launch failures.

/// Error during node launch.
///
/// Wraps both protocol-specific errors and infrastructure errors.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum LaunchError<E: std::error::Error + 'static> {
    /// Protocol-specific error during component build or service spawn.
    #[error("protocol error: {0}")]
    Protocol(E),
    /// Infrastructure error (gRPC, database, etc.).
    #[error("infrastructure error: {0}")]
    Infrastructure(#[from] InfrastructureError),
}

/// Infrastructure-level errors during node launch.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum InfrastructureError {
    /// Failed to build gRPC reflection service.
    #[error("gRPC reflection service: {0}")]
    GrpcReflection(#[from] tonic_reflection::server::Error),
}
