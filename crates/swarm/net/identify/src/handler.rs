//! Connection handler for identify protocol with targeted push support.

use std::{
    collections::HashSet,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use either::Either;
use futures::prelude::*;
use futures_bounded::Timeout;
use libp2p::core::{
    Multiaddr,
    upgrade::{ReadyUpgrade, SelectUpgrade},
};
use libp2p::identity::PeerId;
use libp2p::swarm::{
    ConnectionHandler, ConnectionHandlerEvent, StreamProtocol, StreamUpgradeError,
    SubstreamProtocol, SupportedProtocols,
    handler::{
        ConnectionEvent, DialUpgradeError, FullyNegotiatedInbound, FullyNegotiatedOutbound,
        ProtocolSupport,
    },
};
use smallvec::SmallVec;
use tracing::Level;
use vertex_metrics::StreamGuard;

use crate::{
    PROTOCOL_NAME, PUSH_PROTOCOL_NAME,
    behaviour::KeyType,
    error::UpgradeError,
    protocol::{self, Info, PushInfo},
};

const STREAM_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_CONCURRENT_STREAMS_PER_CONNECTION: usize = 10;

/// Protocol handler for sending and receiving identification requests.
pub struct Handler {
    remote_peer_id: PeerId,
    #[allow(clippy::type_complexity)]
    events: SmallVec<
        [ConnectionHandlerEvent<
            Either<ReadyUpgrade<StreamProtocol>, ReadyUpgrade<StreamProtocol>>,
            (),
            Event,
        >; 4],
    >,
    active_streams: futures_bounded::FuturesSet<Result<Success, UpgradeError>>,
    /// Timer for the initial identify (fires immediately) and optional periodic re-identification.
    trigger_next_identify: Option<futures_timer::Delay>,
    exchanged_one_periodic_identify: bool,
    /// `None` means no periodic re-identification after the initial exchange.
    interval: Option<Duration>,
    local_key: Arc<KeyType>,
    protocol_version: String,
    agent_version: String,
    observed_addr: Multiaddr,
    remote_info: Option<Info>,
    local_supported_protocols: SupportedProtocols,
    remote_supported_protocols: HashSet<StreamProtocol>,
    external_addresses: HashSet<Multiaddr>,
    /// Override addresses for the next push (for targeted push support).
    pending_push_override: Option<Vec<Multiaddr>>,
}

/// Event from behaviour to handler.
#[derive(Debug)]
pub enum InEvent {
    /// External addresses changed.
    AddressesChanged(HashSet<Multiaddr>),
    /// Push identify info to the remote using current external addresses.
    Push,
    /// Push identify info with specific addresses (for targeted push).
    PushWithAddresses(Vec<Multiaddr>),
}

/// Event produced by the handler.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Event {
    /// Received identification from the remote.
    Identified(Info),
    /// Replied to an identification request.
    Identification,
    /// Pushed our identification to the remote.
    IdentificationPushed(Info),
    /// Failed to identify or reply.
    IdentificationError(StreamUpgradeError<UpgradeError>),
}

impl Handler {
    pub(crate) fn new(
        interval: Option<Duration>,
        remote_peer_id: PeerId,
        local_key: Arc<KeyType>,
        protocol_version: String,
        agent_version: String,
        observed_addr: Multiaddr,
        external_addresses: HashSet<Multiaddr>,
    ) -> Self {
        Self {
            remote_peer_id,
            events: SmallVec::new(),
            active_streams: futures_bounded::FuturesSet::new(
                STREAM_TIMEOUT,
                MAX_CONCURRENT_STREAMS_PER_CONNECTION,
            ),
            // Fires immediately to trigger the initial identify exchange.
            trigger_next_identify: Some(futures_timer::Delay::new(Duration::ZERO)),
            exchanged_one_periodic_identify: false,
            interval,
            local_key,
            protocol_version,
            agent_version,
            observed_addr,
            local_supported_protocols: SupportedProtocols::default(),
            remote_supported_protocols: HashSet::default(),
            remote_info: Default::default(),
            external_addresses,
            pending_push_override: None,
        }
    }

    fn on_fully_negotiated_inbound(
        &mut self,
        FullyNegotiatedInbound {
            protocol: output, ..
        }: FullyNegotiatedInbound<<Self as ConnectionHandler>::InboundProtocol>,
    ) {
        match output {
            future::Either::Left(stream) => {
                let info = self.build_info();

                if self
                    .active_streams
                    .try_push(async move {
                        let _guard = StreamGuard::inbound("identify");
                        protocol::send_identify(stream, info)
                            .map_ok(|_| Success::SentIdentify)
                            .await
                    })
                    .is_err()
                {
                    tracing::warn!("Dropping inbound stream because we are at capacity");
                } else {
                    self.exchanged_one_periodic_identify = true;
                }
            }
            future::Either::Right(stream) => {
                if self
                    .active_streams
                    .try_push(async move {
                        let _guard = StreamGuard::inbound("identify");
                        protocol::recv_push(stream)
                            .map_ok(Success::ReceivedIdentifyPush)
                            .await
                    })
                    .is_err()
                {
                    tracing::warn!(
                        "Dropping inbound identify push stream because we are at capacity"
                    );
                }
            }
        }
    }

    fn on_fully_negotiated_outbound(
        &mut self,
        FullyNegotiatedOutbound {
            protocol: output, ..
        }: FullyNegotiatedOutbound<<Self as ConnectionHandler>::OutboundProtocol>,
    ) {
        match output {
            future::Either::Left(stream) => {
                if self
                    .active_streams
                    .try_push(async move {
                        let _guard = StreamGuard::outbound("identify");
                        protocol::recv_identify(stream)
                            .map_ok(Success::ReceivedIdentify)
                            .await
                    })
                    .is_err()
                {
                    tracing::warn!("Dropping outbound identify stream because we are at capacity");
                }
            }
            future::Either::Right(stream) => {
                // Build info, potentially using override addresses
                let info = self.build_info_for_push();

                if self
                    .active_streams
                    .try_push(async move {
                        let _guard = StreamGuard::outbound("identify");
                        protocol::send_identify(stream, info)
                            .map_ok(Success::SentIdentifyPush)
                            .await
                    })
                    .is_err()
                {
                    tracing::warn!(
                        "Dropping outbound identify push stream because we are at capacity"
                    );
                }
            }
        }
    }

    /// Build info for regular identify responses (uses external_addresses).
    fn build_info(&mut self) -> Info {
        self.build_info_with_addresses(Vec::from_iter(self.external_addresses.iter().cloned()))
    }

    /// Build info for push, consuming any pending override addresses.
    fn build_info_for_push(&mut self) -> Info {
        let addresses = self
            .pending_push_override
            .take()
            .unwrap_or_else(|| Vec::from_iter(self.external_addresses.iter().cloned()));
        self.build_info_with_addresses(addresses)
    }

    fn build_info_with_addresses(&self, addresses: Vec<Multiaddr>) -> Info {
        let signed_envelope = match self.local_key.as_ref() {
            KeyType::PublicKey(_) => None,
            KeyType::Keypair { keypair, .. } => {
                libp2p::core::PeerRecord::new(keypair, addresses.clone())
                    .ok()
                    .map(|r| r.into_signed_envelope())
            }
        };
        Info {
            public_key: self.local_key.public_key().clone(),
            protocol_version: self.protocol_version.clone(),
            agent_version: self.agent_version.clone(),
            listen_addrs: addresses,
            protocols: Vec::from_iter(self.local_supported_protocols.iter().cloned()),
            observed_addr: self.observed_addr.clone(),
            signed_peer_record: signed_envelope,
        }
    }

    fn handle_incoming_info(&mut self, info: &Info) -> bool {
        let derived_peer_id = info.public_key.to_peer_id();
        if self.remote_peer_id != derived_peer_id {
            return false;
        }

        self.remote_info.replace(info.clone());
        self.update_supported_protocols_for_remote(info);
        true
    }

    fn update_supported_protocols_for_remote(&mut self, remote_info: &Info) {
        let new_remote_protocols = HashSet::from_iter(remote_info.protocols.clone());

        let remote_added_protocols = new_remote_protocols
            .difference(&self.remote_supported_protocols)
            .cloned()
            .collect::<HashSet<_>>();
        let remote_removed_protocols = self
            .remote_supported_protocols
            .difference(&new_remote_protocols)
            .cloned()
            .collect::<HashSet<_>>();

        if !remote_added_protocols.is_empty() {
            self.events
                .push(ConnectionHandlerEvent::ReportRemoteProtocols(
                    ProtocolSupport::Added(remote_added_protocols),
                ));
        }

        if !remote_removed_protocols.is_empty() {
            self.events
                .push(ConnectionHandlerEvent::ReportRemoteProtocols(
                    ProtocolSupport::Removed(remote_removed_protocols),
                ));
        }

        self.remote_supported_protocols = new_remote_protocols;
    }

    fn local_protocols_to_string(&mut self) -> String {
        self.local_supported_protocols
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl ConnectionHandler for Handler {
    type FromBehaviour = InEvent;
    type ToBehaviour = Event;
    type InboundProtocol =
        SelectUpgrade<ReadyUpgrade<StreamProtocol>, ReadyUpgrade<StreamProtocol>>;
    type OutboundProtocol = Either<ReadyUpgrade<StreamProtocol>, ReadyUpgrade<StreamProtocol>>;
    type OutboundOpenInfo = ();
    type InboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol> {
        SubstreamProtocol::new(
            SelectUpgrade::new(
                ReadyUpgrade::new(PROTOCOL_NAME),
                ReadyUpgrade::new(PUSH_PROTOCOL_NAME),
            ),
            (),
        )
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match event {
            InEvent::AddressesChanged(addresses) => {
                self.external_addresses = addresses;
            }
            InEvent::Push => {
                self.events
                    .push(ConnectionHandlerEvent::OutboundSubstreamRequest {
                        protocol: SubstreamProtocol::new(
                            Either::Right(ReadyUpgrade::new(PUSH_PROTOCOL_NAME)),
                            (),
                        ),
                    });
            }
            InEvent::PushWithAddresses(addresses) => {
                // Store override addresses and trigger push
                self.pending_push_override = Some(addresses);
                self.events
                    .push(ConnectionHandlerEvent::OutboundSubstreamRequest {
                        protocol: SubstreamProtocol::new(
                            Either::Right(ReadyUpgrade::new(PUSH_PROTOCOL_NAME)),
                            (),
                        ),
                    });
            }
        }
    }

    #[tracing::instrument(level = "trace", name = "ConnectionHandler::poll", skip(self, cx))]
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ConnectionHandlerEvent<Self::OutboundProtocol, (), Event>> {
        if let Some(event) = self.events.pop() {
            return Poll::Ready(event);
        }

        if let Some(delay) = self.trigger_next_identify.as_mut()
            && let Poll::Ready(()) = delay.poll_unpin(cx)
        {
            // After the initial identify, only schedule another if periodic is enabled.
            match self.interval {
                Some(interval) => delay.reset(interval),
                None => self.trigger_next_identify = None,
            }
            let event = ConnectionHandlerEvent::OutboundSubstreamRequest {
                protocol: SubstreamProtocol::new(
                    Either::Left(ReadyUpgrade::new(PROTOCOL_NAME)),
                    (),
                ),
            };
            return Poll::Ready(event);
        }

        while let Poll::Ready(ready) = self.active_streams.poll_unpin(cx) {
            match ready {
                Ok(Ok(Success::ReceivedIdentify(remote_info))) => {
                    if self.handle_incoming_info(&remote_info) {
                        return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                            Event::Identified(remote_info),
                        ));
                    } else {
                        tracing::warn!(
                            %self.remote_peer_id,
                            ?remote_info.public_key,
                            derived_peer_id=%remote_info.public_key.to_peer_id(),
                            "Discarding received identify message as public key does not match remote peer ID",
                        );
                    }
                }
                Ok(Ok(Success::SentIdentifyPush(info))) => {
                    return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                        Event::IdentificationPushed(info),
                    ));
                }
                Ok(Ok(Success::SentIdentify)) => {
                    return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                        Event::Identification,
                    ));
                }
                Ok(Ok(Success::ReceivedIdentifyPush(remote_push_info))) => {
                    if let Some(mut info) = self.remote_info.clone() {
                        info.merge(remote_push_info);

                        if self.handle_incoming_info(&info) {
                            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                                Event::Identified(info),
                            ));
                        } else {
                            tracing::warn!(
                                %self.remote_peer_id,
                                ?info.public_key,
                                derived_peer_id=%info.public_key.to_peer_id(),
                                "Discarding received identify message as public key does not match remote peer ID",
                            );
                        }
                    }
                }
                Ok(Err(e)) => {
                    return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                        Event::IdentificationError(StreamUpgradeError::Apply(e)),
                    ));
                }
                Err(Timeout { .. }) => {
                    return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                        Event::IdentificationError(StreamUpgradeError::Timeout),
                    ));
                }
            }
        }

        Poll::Pending
    }

    fn on_connection_event(
        &mut self,
        event: ConnectionEvent<Self::InboundProtocol, Self::OutboundProtocol>,
    ) {
        match event {
            ConnectionEvent::FullyNegotiatedInbound(fully_negotiated_inbound) => {
                self.on_fully_negotiated_inbound(fully_negotiated_inbound)
            }
            ConnectionEvent::FullyNegotiatedOutbound(fully_negotiated_outbound) => {
                self.on_fully_negotiated_outbound(fully_negotiated_outbound)
            }
            // ReadyUpgrade never fails, so the upgrade error is Infallible (void).
            // The unreachable() call consumes a void value, which triggers unreachable_code.
            // This follows the upstream libp2p pattern for ReadyUpgrade error handling.
            #[allow(unreachable_code)]
            ConnectionEvent::DialUpgradeError(DialUpgradeError { error, .. }) => {
                self.events.push(ConnectionHandlerEvent::NotifyBehaviour(
                    Event::IdentificationError(
                        error.map_upgrade_err(|e| libp2p::core::util::unreachable(e.into_inner())),
                    ),
                ));
                // Retry after error only if periodic is enabled.
                if let Some(interval) = self.interval {
                    self.trigger_next_identify = Some(futures_timer::Delay::new(interval));
                }
            }
            ConnectionEvent::LocalProtocolsChange(change) => {
                let before = tracing::enabled!(Level::DEBUG)
                    .then(|| self.local_protocols_to_string())
                    .unwrap_or_default();
                let protocols_changed = self.local_supported_protocols.on_protocols_change(change);
                let after = tracing::enabled!(Level::DEBUG)
                    .then(|| self.local_protocols_to_string())
                    .unwrap_or_default();

                if protocols_changed && self.exchanged_one_periodic_identify {
                    tracing::debug!(
                        peer=%self.remote_peer_id,
                        %before,
                        %after,
                        "Supported listen protocols changed, pushing to peer"
                    );

                    self.events
                        .push(ConnectionHandlerEvent::OutboundSubstreamRequest {
                            protocol: SubstreamProtocol::new(
                                Either::Right(ReadyUpgrade::new(PUSH_PROTOCOL_NAME)),
                                (),
                            ),
                        });
                }
            }
            _ => {}
        }
    }
}

enum Success {
    SentIdentify,
    ReceivedIdentify(Info),
    SentIdentifyPush(Info),
    ReceivedIdentifyPush(PushInfo),
}
