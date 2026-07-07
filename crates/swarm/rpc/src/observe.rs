//! Error boundary for the gRPC services: the single site where a [`SwarmError`]
//! becomes a [`Status`], preserving the error taxonomy.
//!
//! The reason label is the error's `IntoStaticStr` discriminant; it is recorded
//! and logged here, once, before the status leaves the server. Deeper layers
//! (the stream combinator, the providers) return errors without logging so the
//! same failure is not logged twice.

use tonic::{Code, Status};
use tracing::warn;
use vertex_swarm_api::SwarmError;

/// Convert a [`SwarmError`] to a [`Status`] at the RPC boundary.
///
/// Records `grpc_errors_total{method, reason}` with the taxonomy label and logs
/// the failure once with its source chain.
pub(crate) fn boundary_status(method: &'static str, error: SwarmError) -> Status {
    let reason: &'static str = (&error).into();
    let code = status_code(&error);

    metrics::counter!("grpc_errors_total", "method" => method, "reason" => reason).increment(1);
    warn!(
        method,
        reason,
        code = code as i32,
        error = %error,
        "gRPC request failed",
    );

    Status::new(code, error.to_string())
}

/// Map a [`SwarmError`] to a gRPC status code, keeping absence retryable.
///
/// Exhausted retrieval is `unavailable` (forwarding retrieval cannot prove
/// absence, so the caller may retry), a missing storer is `not_found`, invalid
/// input is `invalid_argument`, and bandwidth or payment pressure is
/// `resource_exhausted`; the rest are `internal`.
fn status_code(error: &SwarmError) -> Code {
    match error {
        SwarmError::RetrievalExhausted { .. }
        | SwarmError::PeerUnavailable { .. }
        | SwarmError::Network { .. } => Code::Unavailable,
        SwarmError::NoStorer { .. } => Code::NotFound,
        SwarmError::InvalidChunk { .. } | SwarmError::InvalidSignature { .. } => {
            Code::InvalidArgument
        }
        SwarmError::BandwidthLimitExceeded { .. } | SwarmError::PaymentRequired { .. } => {
            Code::ResourceExhausted
        }
        _ => Code::Internal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_api::ChunkAddress;

    #[test]
    fn retrieval_exhausted_is_unavailable() {
        let err = SwarmError::RetrievalExhausted {
            address: ChunkAddress::default(),
        };
        assert_eq!(status_code(&err), Code::Unavailable);
    }

    #[test]
    fn no_storer_is_not_found() {
        let err = SwarmError::NoStorer {
            chunk_address: ChunkAddress::default(),
        };
        assert_eq!(status_code(&err), Code::NotFound);
    }

    #[test]
    fn storage_is_internal() {
        let err = SwarmError::storage_msg("disk full");
        assert_eq!(status_code(&err), Code::Internal);
    }

    #[test]
    fn boundary_status_carries_display_message() {
        let status = boundary_status(
            "retrieve_chunk",
            SwarmError::NoStorer {
                chunk_address: ChunkAddress::default(),
            },
        );
        assert_eq!(status.code(), Code::NotFound);
        assert!(status.message().contains("no storer"));
    }
}
