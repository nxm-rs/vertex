//! Combined upgrades for the two pullsync substreams.
//!
//! Inbound advertises both [`PROTOCOL_CURSORS`] and [`PROTOCOL_SYNC`] and
//! dispatches on the negotiated id. Outbound knows its id from the command.

use futures::future::BoxFuture;
use libp2p::{InboundUpgrade, OutboundUpgrade, Stream, core::UpgradeInfo};
use vertex_swarm_net_headers::ProtocolError;
use vertex_swarm_net_pullsync::{
    Ack, CursorsResponder, Get, Offer, PROTOCOL_CURSORS, PROTOCOL_SYNC, SyncRequester,
    SyncResponder, cursors_inbound, cursors_outbound, sync_inbound, sync_outbound,
};

/// Output of the inbound upgrade once a substream negotiates.
pub enum InboundOutput {
    /// Cursor handshake: `Syn` read, awaiting our `Ack`.
    Cursors(CursorsResponder),
    /// Range request: the `Get`, plus the responder for offer/want/delivery.
    Sync(Get, SyncResponder),
}

/// Inbound upgrade for both pullsync substreams.
#[derive(Clone, Debug, Default)]
pub struct PullsyncInboundUpgrade;

impl UpgradeInfo for PullsyncInboundUpgrade {
    type Info = &'static str;
    type InfoIter = std::array::IntoIter<Self::Info, 2>;

    fn protocol_info(&self) -> Self::InfoIter {
        [PROTOCOL_CURSORS, PROTOCOL_SYNC].into_iter()
    }
}

impl InboundUpgrade<Stream> for PullsyncInboundUpgrade {
    type Output = InboundOutput;
    type Error = ProtocolError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, socket: Stream, info: Self::Info) -> Self::Future {
        Box::pin(async move {
            match info {
                PROTOCOL_CURSORS => {
                    let responder = cursors_inbound().upgrade_inbound(socket, info).await?;
                    Ok(InboundOutput::Cursors(responder))
                }
                _ => {
                    let (get, responder) = sync_inbound().upgrade_inbound(socket, info).await?;
                    Ok(InboundOutput::Sync(get, responder))
                }
            }
        })
    }
}

/// Output of the outbound upgrade once a substream negotiates.
#[allow(clippy::large_enum_variant)]
pub enum OutboundOutput {
    /// Cursor handshake answered with the peer's `Ack`.
    Cursors(Ack),
    /// Range request answered with the `Offer`, plus the requester for
    /// want/delivery.
    Sync(Offer, SyncRequester),
}

/// Outbound upgrade selecting one pullsync substream per command.
pub enum PullsyncOutboundUpgrade {
    /// Open the cursor handshake.
    Cursors,
    /// Open a range exchange for the given `Get`.
    Sync(Get),
}

impl UpgradeInfo for PullsyncOutboundUpgrade {
    type Info = &'static str;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        let name = match self {
            Self::Cursors => PROTOCOL_CURSORS,
            Self::Sync(_) => PROTOCOL_SYNC,
        };
        std::iter::once(name)
    }
}

impl OutboundUpgrade<Stream> for PullsyncOutboundUpgrade {
    type Output = OutboundOutput;
    type Error = ProtocolError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_outbound(self, socket: Stream, info: Self::Info) -> Self::Future {
        Box::pin(async move {
            match self {
                Self::Cursors => {
                    let ack = cursors_outbound().upgrade_outbound(socket, info).await?;
                    Ok(OutboundOutput::Cursors(ack))
                }
                Self::Sync(get) => {
                    let (offer, requester) =
                        sync_outbound(get).upgrade_outbound(socket, info).await?;
                    Ok(OutboundOutput::Sync(offer, requester))
                }
            }
        })
    }
}
