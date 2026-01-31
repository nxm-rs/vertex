//! Protocol upgrade for SWAP.
//!
//! SWAP is a settlement protocol with header-based rate negotiation (headler pattern):
//! - Headers are exchanged first with exchange rates
//! - Initiator sends `EmitCheque` with the signed cheque
//! - No response message (rate negotiation happens via headers)

use std::collections::HashMap;

use asynchronous_codec::Framed;
use bytes::Bytes;
use futures::{SinkExt, TryStreamExt, future::BoxFuture};
use tracing::debug;
use vertex_bandwidth_chequebook::SignedCheque;
use vertex_net_headers::{HeaderedInbound, HeaderedOutbound, HeaderedStream, Inbound, Outbound};

use crate::{
    PROTOCOL_NAME,
    codec::{EmitCheque, EmitChequeCodec, SwapCodecError},
    headers::{SettlementHeaders, HEADER_EXCHANGE_RATE},
};

const MAX_MESSAGE_SIZE: usize = 4096;

/// SWAP inbound handler.
///
/// Receives a cheque from the remote peer with exchange rate negotiation via headers.
#[derive(Debug, Clone)]
pub struct SwapInboundInner {
    our_rate: alloy_primitives::U256,
}

impl SwapInboundInner {
    pub fn new(our_rate: alloy_primitives::U256) -> Self {
        Self { our_rate }
    }
}

impl HeaderedInbound for SwapInboundInner {
    type Output = (SignedCheque, SettlementHeaders);
    type Error = SwapCodecError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn response_headers(&self, peer_headers: &HashMap<String, Bytes>) -> HashMap<String, Bytes> {
        if let Some(peer_rate_bytes) = peer_headers.get(HEADER_EXCHANGE_RATE) {
            if let Some(peer_rate) = parse_u256_bytes(peer_rate_bytes) {
                debug!(peer_rate = %peer_rate, our_rate = %self.our_rate, "SWAP: Negotiating rate");
            }
        }
        SettlementHeaders::with_rate(self.our_rate).to_headers()
    }

    fn read(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            let peer_headers = SettlementHeaders::from_headers(stream.headers())
                .ok_or_else(|| SwapCodecError::Protocol("missing exchange rate header".into()))?;

            let codec = EmitChequeCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(stream.into_inner(), codec);

            debug!("SWAP: Reading cheque");
            let emit = framed.try_next().await?.ok_or_else(|| {
                SwapCodecError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed before cheque received",
                ))
            })?;

            debug!("SWAP: Received cheque");
            Ok((emit.cheque, peer_headers))
        })
    }
}

/// SWAP outbound handler.
///
/// Sends a cheque to the remote peer with exchange rate negotiation via headers.
#[derive(Debug, Clone)]
pub struct SwapOutboundInner {
    cheque: SignedCheque,
    our_rate: alloy_primitives::U256,
}

impl SwapOutboundInner {
    pub fn new(cheque: SignedCheque, our_rate: alloy_primitives::U256) -> Self {
        Self { cheque, our_rate }
    }
}

impl HeaderedOutbound for SwapOutboundInner {
    type Output = SettlementHeaders;
    type Error = SwapCodecError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn headers(&self) -> HashMap<String, Bytes> {
        SettlementHeaders::with_rate(self.our_rate).to_headers()
    }

    fn write(
        self,
        stream: HeaderedStream,
    ) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            let peer_headers = SettlementHeaders::from_headers(stream.headers())
                .ok_or_else(|| SwapCodecError::Protocol("missing exchange rate header".into()))?;

            debug!(peer_rate = %peer_headers.exchange_rate, our_rate = %self.our_rate, "SWAP: Peer rate");

            let codec = EmitChequeCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(stream.into_inner(), codec);

            debug!("SWAP: Sending cheque");
            framed.send(EmitCheque::new(self.cheque)).await?;

            Ok(peer_headers)
        })
    }
}

pub type SwapInboundProtocol = Inbound<SwapInboundInner>;
pub type SwapOutboundProtocol = Outbound<SwapOutboundInner>;

pub fn inbound(our_rate: alloy_primitives::U256) -> SwapInboundProtocol {
    Inbound::new(SwapInboundInner::new(our_rate))
}

pub fn outbound(cheque: SignedCheque, our_rate: alloy_primitives::U256) -> SwapOutboundProtocol {
    Outbound::new(SwapOutboundInner::new(cheque, our_rate))
}

fn parse_u256_bytes(bytes: &Bytes) -> Option<alloy_primitives::U256> {
    if bytes.is_empty() {
        return Some(alloy_primitives::U256::ZERO);
    }
    if bytes.len() > 32 {
        return None;
    }
    Some(alloy_primitives::U256::from_be_slice(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, U256};
    use vertex_bandwidth_chequebook::{Cheque, ChequeExt};

    fn test_signed_cheque() -> SignedCheque {
        let cheque = Cheque::new(
            Address::repeat_byte(0x01),
            Address::repeat_byte(0x02),
            U256::from(1_000_000u64),
        );
        SignedCheque::new(cheque, Bytes::from(vec![0u8; 65]))
    }

    #[test]
    fn test_emit_cheque_creation() {
        let cheque = test_signed_cheque();
        let emit = EmitCheque::new(cheque.clone());
        assert_eq!(emit.cheque, cheque);
    }

    #[test]
    fn test_settlement_headers_to_from() {
        let headers = SettlementHeaders::with_rate(U256::from(1_000_000u64));
        let map = headers.to_headers();
        let parsed = SettlementHeaders::from_headers(&map).unwrap();
        assert_eq!(headers, parsed);
    }

    #[test]
    fn test_inbound_response_headers() {
        let inbound = SwapInboundInner::new(U256::from(500_000u64));
        let mut peer_headers = HashMap::new();
        peer_headers.insert(
            HEADER_EXCHANGE_RATE.to_string(),
            Bytes::from(U256::from(600_000u64).to_be_bytes::<32>().to_vec()),
        );

        let response = inbound.response_headers(&peer_headers);
        let parsed = SettlementHeaders::from_headers(&response).unwrap();
        assert_eq!(parsed.exchange_rate, U256::from(500_000u64));
    }
}
