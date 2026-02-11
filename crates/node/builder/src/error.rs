//! Error types for node launch failures.

use std::fmt;

/// Error during node launch.
///
/// Wraps both protocol-specific errors and infrastructure errors.
#[derive(Debug)]
pub enum LaunchError<E> {
    /// Protocol-specific error during component build or service spawn.
    Protocol(E),
    /// Infrastructure error (gRPC, database, etc.).
    Infrastructure(InfrastructureError),
}

impl<E: fmt::Display> fmt::Display for LaunchError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(e) => write!(f, "protocol error: {e}"),
            Self::Infrastructure(e) => write!(f, "infrastructure error: {e}"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for LaunchError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Protocol(e) => Some(e),
            Self::Infrastructure(e) => Some(e),
        }
    }
}

impl<E> From<InfrastructureError> for LaunchError<E> {
    fn from(e: InfrastructureError) -> Self {
        Self::Infrastructure(e)
    }
}

/// Infrastructure-level errors during node launch.
#[derive(Debug)]
pub enum InfrastructureError {
    /// Failed to build gRPC reflection service.
    GrpcReflection(tonic_reflection::server::Error),
}

impl fmt::Display for InfrastructureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GrpcReflection(e) => write!(f, "gRPC reflection service: {e}"),
        }
    }
}

impl std::error::Error for InfrastructureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::GrpcReflection(e) => Some(e),
        }
    }
}

impl From<tonic_reflection::server::Error> for InfrastructureError {
    fn from(e: tonic_reflection::server::Error) -> Self {
        Self::GrpcReflection(e)
    }
}
