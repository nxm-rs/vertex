//! Protocol upgrade for pseudosettle.
//!
//! Pseudosettle is a request/response protocol with typed message exchange:
//! - Initiator sends `Payment` -> receives `PaymentAck`
//! - Responder receives `Payment` -> sends `PaymentAck`
//!
//! The protocol uses separate typed codecs for each message type, enforcing
//! type safety at each step. Codec switching is done via `Framed::into_parts()`
//! to extract the raw stream and create a new Framed with the appropriate codec.

use asynchronous_codec::Framed;
use futures::{SinkExt, TryStreamExt, future::BoxFuture};
use tracing::debug;
use vertex_net_headers::{HeaderedInbound, HeaderedOutbound, HeaderedStream, Inbound, Outbound};

use crate::{
    PROTOCOL_NAME,
    codec::{
        Payment, PaymentAck, PaymentAckCodec, PaymentCodec, PseudosettleCodecError,
        PseudosettleError,
    },
};

const MAX_MESSAGE_SIZE: usize = 1024;

/// Pseudosettle inbound handler.
///
/// Receives a `Payment` from the remote peer and returns both the payment
/// and a responder for sending the `PaymentAck`.
#[derive(Debug, Clone, Default)]
pub struct PseudosettleInboundInner;

impl PseudosettleInboundInner {
    pub fn new() -> Self {
        Self
    }
}

/// Result of handling an inbound pseudosettle request.
pub struct PseudosettleInboundResult {
    /// The payment request received from the peer.
    pub payment: Payment,
    responder: PseudosettleResponder,
}

impl PseudosettleInboundResult {
    /// Send the acknowledgment response.
    pub async fn respond(self, ack: PaymentAck) -> Result<(), PseudosettleCodecError> {
        self.responder.send_ack(ack).await
    }

    /// Send an acknowledgment with the same amount and current timestamp.
    pub async fn ack_now(self) -> Result<(), PseudosettleCodecError> {
        let ack = PaymentAck::now(self.payment.amount);
        self.respond(ack).await
    }
}

/// Helper for sending the PaymentAck response.
pub struct PseudosettleResponder {
    stream: libp2p::Stream,
}

impl PseudosettleResponder {
    pub async fn send_ack(self, ack: PaymentAck) -> Result<(), PseudosettleCodecError> {
        let codec = PaymentAckCodec::new(MAX_MESSAGE_SIZE);
        let mut framed = Framed::new(self.stream, codec);

        debug!(amount = %ack.amount, timestamp = ack.timestamp, "Pseudosettle: Sending ack");
        framed.send(ack).await?;
        Ok(())
    }
}

impl HeaderedInbound for PseudosettleInboundInner {
    type Output = PseudosettleInboundResult;
    type Error = PseudosettleCodecError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn read(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            let raw_stream = stream.into_inner();
            let codec = PaymentCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(raw_stream, codec);

            debug!("Pseudosettle: Reading payment request");
            let payment = framed.try_next().await?.ok_or_else(|| {
                PseudosettleCodecError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed before payment received",
                ))
            })?;

            debug!(amount = %payment.amount, "Pseudosettle: Received payment");

            let parts = framed.into_parts();
            let responder = PseudosettleResponder { stream: parts.io };

            Ok(PseudosettleInboundResult { payment, responder })
        })
    }
}

/// Pseudosettle outbound handler.
///
/// Sends a `Payment` to the remote peer and waits for a `PaymentAck` response.
#[derive(Debug, Clone)]
pub struct PseudosettleOutboundInner {
    payment: Payment,
}

impl PseudosettleOutboundInner {
    pub fn new(payment: Payment) -> Self {
        Self { payment }
    }
}

impl HeaderedOutbound for PseudosettleOutboundInner {
    type Output = PaymentAck;
    type Error = PseudosettleCodecError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn write(
        self,
        stream: HeaderedStream,
    ) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            let raw_stream = stream.into_inner();

            // Send Payment
            let codec = PaymentCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(raw_stream, codec);

            debug!(amount = %self.payment.amount, "Pseudosettle: Sending payment");
            framed.send(self.payment).await?;

            // Switch codec to read PaymentAck
            let parts = framed.into_parts();
            let codec = PaymentAckCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(parts.io, codec);

            debug!("Pseudosettle: Waiting for ack");
            let ack = framed.try_next().await?.ok_or_else(|| {
                PseudosettleCodecError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed before ack received",
                ))
            })?;

            debug!(amount = %ack.amount, timestamp = ack.timestamp, "Pseudosettle: Received ack");
            Ok(ack)
        })
    }
}

pub type PseudosettleInboundProtocol = Inbound<PseudosettleInboundInner>;
pub type PseudosettleOutboundProtocol = Outbound<PseudosettleOutboundInner>;

pub fn inbound() -> PseudosettleInboundProtocol {
    Inbound::new(PseudosettleInboundInner::new())
}

pub fn outbound(payment: Payment) -> PseudosettleOutboundProtocol {
    Outbound::new(PseudosettleOutboundInner::new(payment))
}

/// Validate that a timestamp is within acceptable bounds of the current time.
pub fn validate_timestamp(
    timestamp: i64,
    tolerance_secs: u64,
) -> Result<(), PseudosettleCodecError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos() as i64;

    let tolerance_nanos = (tolerance_secs as i64) * 1_000_000_000;
    let diff = (now - timestamp).abs();

    if diff > tolerance_nanos {
        return Err(PseudosettleCodecError::domain(
            PseudosettleError::InvalidTimestamp(format!(
                "timestamp {} is {}s away from current time (tolerance: {}s)",
                timestamp,
                diff / 1_000_000_000,
                tolerance_secs
            )),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;

    #[test]
    fn test_validate_timestamp_valid() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;

        assert!(validate_timestamp(now, 3).is_ok());
        assert!(validate_timestamp(now - 1_000_000_000, 3).is_ok());
        assert!(validate_timestamp(now + 2_000_000_000, 3).is_ok());
    }

    #[test]
    fn test_validate_timestamp_invalid() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;

        assert!(validate_timestamp(now - 5_000_000_000, 3).is_err());
        assert!(validate_timestamp(now + 10_000_000_000, 3).is_err());
    }

    #[test]
    fn test_payment_creation() {
        let payment = Payment::from_u64(1_000_000);
        assert_eq!(payment.amount, U256::from(1_000_000u64));
    }

    #[test]
    fn test_payment_ack_now() {
        let amount = U256::from(500_000u64);
        let ack = PaymentAck::now(amount);
        assert_eq!(ack.amount, amount);
        assert!(validate_timestamp(ack.timestamp, 1).is_ok());
    }
}
