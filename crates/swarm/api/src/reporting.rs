//! Peer reporting, admission, and lifecycle event types.
//!
//! These types are the shared vocabulary between the peer manager and the
//! subsystems that observe or judge peers:
//!
//! - [`PeerReporter`] is the single sanctioned path for scoring input. The
//!   peer manager implements it via its handle; topology, gossip,
//!   handshake, protocol handlers, bandwidth accounting, and the RPC
//!   surface all report through it instead of mutating scores directly.
//! - [`Ledger`] and [`AdmissionControl`] are implemented by bandwidth
//!   accounting so that selection and pacing can read per-peer balances and
//!   band a priced request before doing the work.
//! - [`PeerLifecycleEvent`] is emitted by the peer manager; topology
//!   subscribes to it and executes the resulting disconnects and dial
//!   policy changes.

use core::time::Duration;

use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};

use crate::{Admission, Au, Debt, Threshold};

/// Peer scoring events reported by subsystems.
///
/// Each event carries an implicit weight (see [`default_weight`]) that is
/// applied to the peer's score by the scoring engine. Concrete weight
/// configuration lives with the scoring implementation; this enum only
/// names the observable behaviours.
///
/// [`default_weight`]: SwarmScoringEvent::default_weight
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum SwarmScoringEvent {
    /// Successful connection with optional latency.
    ConnectionSuccess {
        /// Time taken to establish the connection, if measured.
        latency: Option<Duration>,
    },
    /// Connection attempt timed out.
    ConnectionTimeout,
    /// Connection was refused by peer.
    ConnectionRefused,
    /// Handshake protocol failed.
    HandshakeFailure,
    /// Protocol-level error during communication.
    ProtocolError,
    /// Peer disconnected shortly after completing handshake (connection instability).
    EarlyDisconnect {
        /// How long the connection lasted before the peer disconnected.
        duration: Duration,
    },
    /// Successful chunk retrieval.
    RetrievalSuccess {
        /// Time taken to retrieve the chunk.
        latency: Duration,
    },
    /// Chunk retrieval failed.
    RetrievalFailure,
    /// Successful chunk push.
    PushSuccess {
        /// Time taken to push the chunk.
        latency: Duration,
    },
    /// Chunk push failed.
    PushFailure,
    /// Peer provided invalid data (chunk, signature, etc.).
    InvalidData,
    /// Peer is behaving maliciously.
    MaliciousBehavior,
    /// Bandwidth accounting violation.
    AccountingViolation,
    /// Peer exceeded rate limits.
    RateLimitExceeded,
    /// Successful ping/pong.
    PingSuccess {
        /// Round-trip time of the ping.
        latency: Duration,
    },
    /// Ping timed out.
    PingTimeout,
    /// Hive gossip received useful peers.
    GossipUseful,
    /// Hive gossip contained stale/invalid peers.
    GossipStale,
}

impl SwarmScoringEvent {
    /// Get the default weight for this event.
    ///
    /// Positive weights improve score, negative weights decrease it. These
    /// are default values; scoring implementations may apply configured
    /// overrides instead.
    #[must_use]
    pub fn default_weight(&self) -> f64 {
        match self {
            Self::ConnectionSuccess { .. } => 1.0,
            Self::ConnectionTimeout => -1.5,
            Self::ConnectionRefused => -1.0,
            Self::HandshakeFailure => -5.0,
            Self::ProtocolError => -3.0,
            Self::EarlyDisconnect { .. } => -3.0,
            Self::RetrievalSuccess { .. } => 0.5,
            Self::RetrievalFailure => -2.0,
            Self::PushSuccess { .. } => 0.5,
            Self::PushFailure => -2.0,
            Self::InvalidData => -10.0,
            Self::MaliciousBehavior => -50.0,
            Self::AccountingViolation => -20.0,
            Self::RateLimitExceeded => -5.0,
            Self::PingSuccess { .. } => 0.1,
            Self::PingTimeout => -0.5,
            Self::GossipUseful => 0.2,
            Self::GossipStale => -0.1,
        }
    }

    /// Extract latency if this event includes timing information.
    #[must_use]
    pub fn latency(&self) -> Option<Duration> {
        match self {
            Self::ConnectionSuccess { latency } => *latency,
            Self::RetrievalSuccess { latency }
            | Self::PushSuccess { latency }
            | Self::PingSuccess { latency } => Some(*latency),
            _ => None,
        }
    }

    /// True for successful connection events.
    #[must_use]
    pub fn is_connection_success(&self) -> bool {
        matches!(self, Self::ConnectionSuccess { .. })
    }

    /// True for connection timeout events.
    #[must_use]
    pub fn is_connection_timeout(&self) -> bool {
        matches!(self, Self::ConnectionTimeout)
    }

    /// True for protocol error events.
    #[must_use]
    pub fn is_protocol_error(&self) -> bool {
        matches!(self, Self::ProtocolError)
    }

    /// True for events that should trigger an immediate ban check.
    #[must_use]
    pub fn is_severe(&self) -> bool {
        matches!(
            self,
            Self::InvalidData | Self::MaliciousBehavior | Self::AccountingViolation
        )
    }
}

/// Subsystem that originated a peer report.
///
/// Carried alongside every [`SwarmScoringEvent`] so the peer manager can
/// attribute score changes in logs and metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ReportSource {
    /// Topology management (dialing, connection lifecycle, kademlia).
    Topology,
    /// Hive gossip.
    Gossip,
    /// Handshake protocol.
    Handshake,
    /// A wire protocol handler, identified by protocol name.
    Protocol(&'static str),
    /// Bandwidth accounting.
    Accounting,
    /// Operator action over the RPC surface.
    Rpc,
}

/// Report peer behaviour to the authority that owns peer records.
///
/// This is the single sanctioned path for any subsystem to affect a peer's
/// score. The peer manager implements it via its handle; scoring, threshold
/// checks, and the resulting [`PeerLifecycleEvent`]s all happen behind this
/// trait.
#[auto_impl::auto_impl(&, Arc)]
pub trait PeerReporter: Send + Sync {
    /// Report a scoring event for the peer identified by `overlay`.
    fn report_peer(&self, overlay: &OverlayAddress, event: SwarmScoringEvent, source: ReportSource);
}

/// The per-peer ledger reads admission and pacing consume.
///
/// Implemented by bandwidth accounting. `headroom` unifies the two allowance
/// reads (toward each [`Threshold`]) and is floored at zero for the self-throttle.
/// The admission boundary in [`AdmissionControl::admit`] reasons in [`Debt`]
/// against the raw, non-floored thresholds [`disconnect_line`](Self::disconnect_line)
/// and [`settle_trigger`](Self::settle_trigger): a floored headroom collapses to the
/// current debt once the debt is over a threshold, which would silently stop banding.
/// Balances and amounts are in accounting units (AU).
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait Ledger: Send + Sync {
    /// Signed balance: positive means the peer owes us, negative we owe them.
    fn balance(&self, peer: &OverlayAddress) -> Au;

    /// The outstanding receive reservation against this peer, in AU.
    fn reserved(&self, peer: &OverlayAddress) -> Au;

    /// Floored allowance toward `to` before our debt reaches that threshold.
    /// Used only by the self-throttle; admission uses the raw boundaries below.
    fn headroom(&self, peer: &OverlayAddress, to: Threshold) -> Au;

    /// The raw disconnect threshold for `peer`, not floored. A projected debt
    /// strictly past this is refused.
    fn disconnect_line(&self, peer: &OverlayAddress) -> Au;

    /// The raw early-payment settle trigger for `peer`, not floored at the
    /// current debt. A projected debt strictly past this (but within the
    /// disconnect line) settles.
    fn settle_trigger(&self, peer: &OverlayAddress) -> Au;
}

/// The single admission band over a [`Ledger`].
///
/// `admit` is the one boundary expression: it projects the debt this request
/// would create and bands it against the payment and disconnect thresholds. The
/// hard gate (`prepare_receive`) calls this, so the advisory query and the gate
/// can never diverge. The blanket impl gives every ledger the band for free; do
/// not add a second impl (the blanket already covers all `T: Ledger`).
pub trait AdmissionControl: Ledger {
    /// Band a priced request for `peer`.
    fn admit(&self, peer: &OverlayAddress, price: Au) -> Admission {
        let projected = Debt::project(self.balance(peer), self.reserved(peer), price);
        if projected.exceeds(self.disconnect_line(peer)) {
            Admission::Refuse
        } else if projected.exceeds(self.settle_trigger(peer)) {
            Admission::SettleAndAdmit
        } else {
            Admission::Admit
        }
    }
}

impl<T: Ledger> AdmissionControl for T {}

/// Why a connection to a peer closed.
///
/// One vocabulary for both the intent recorded before the node issues a close
/// and the attribution derived afterwards. Every variant except [`RemoteClose`]
/// is locally initiated: the node either chose the close or tore down an idle
/// connection. [`RemoteClose`] is the only peer-attributable case, so the
/// early-disconnect penalty keys on [`Self::is_locally_initiated`].
///
/// The node records the precise intent at every close site; the close handler
/// reads it back and falls through to the libp2p cause only when no intent was
/// recorded ([`IdleTimeout`] for a keep-alive teardown, [`RemoteClose`] for a
/// peer or transport close, [`LocalClose`] for an untagged local close).
///
/// [`RemoteClose`]: Self::RemoteClose
/// [`IdleTimeout`]: Self::IdleTimeout
/// [`LocalClose`]: Self::LocalClose
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum DisconnectReason {
    /// Evicted to rebalance an overpopulated bin after a depth change.
    BinTrimmed,
    /// Inbound connection refused because its bin was saturated.
    BinSaturated,
    /// Connection to a banned peer was closed.
    Banned,
    /// Score fell below the disconnect threshold.
    LowScore,
    /// A protocol handler reported a violation.
    ProtocolViolation,
    /// The peer exhausted its bandwidth allowance.
    AllowanceExceeded,
    /// Disconnect requested by an operator over the RPC surface.
    Requested,
    /// Replaced by a newer connection from the same peer.
    DuplicateConnection,
    /// Bootnode dropped after its initial hive gossip batch.
    BootnodeRotation,
    /// The node is shutting down.
    ShuttingDown,
    /// Local idle teardown (libp2p keep-alive timeout). Not a peer fault.
    IdleTimeout,
    /// A local close with no recorded intent. Rare; indicates a close path
    /// that did not record its reason.
    LocalClose,
    /// The peer or the transport closed the connection. A graceful remote
    /// close and a transport reset are indistinguishable at this layer; this
    /// is the only peer-attributable reason.
    RemoteClose,
}

impl DisconnectReason {
    /// Whether the local node initiated the close.
    ///
    /// Only [`RemoteClose`](Self::RemoteClose) is attributable to the peer;
    /// every other reason is the node's own action or an idle teardown, so the
    /// early-disconnect penalty skips them.
    #[must_use]
    pub fn is_locally_initiated(self) -> bool {
        !matches!(self, Self::RemoteClose)
    }
}

/// Why a peer was banned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum BanCause {
    /// Score fell below the ban threshold.
    LowScore,
    /// The peer provided data that failed validation.
    InvalidData,
    /// The peer behaved maliciously.
    Malicious,
    /// Ban requested by an operator over the RPC surface.
    Requested,
}

/// Lifecycle events emitted by the authority that owns peer records.
///
/// Topology subscribes to this stream and executes the network-side
/// consequences: closing connections for [`DisconnectRequested`], applying
/// dial backoff, and refusing dials to banned peers.
///
/// [`DisconnectRequested`]: PeerLifecycleEvent::DisconnectRequested
#[derive(Debug, Clone)]
pub enum PeerLifecycleEvent {
    /// A peer completed the handshake and is connected.
    Connected {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The node type the peer advertised.
        node_type: SwarmNodeType,
    },
    /// A peer disconnected.
    Disconnected {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// Why the connection closed.
        reason: DisconnectReason,
    },
    /// A peer's score crossed the warn threshold; exclude it from selection.
    ScoreWarning {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The score at the time the threshold was crossed.
        score: f64,
    },
    /// The peer's connection should be closed and the peer backed off.
    DisconnectRequested {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// Why the disconnect was requested.
        reason: DisconnectReason,
    },
    /// The peer was banned.
    Banned {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// Unix timestamp in seconds at which the ban expires, or `None` for
        /// a ban with no scheduled expiry.
        until: Option<u64>,
        /// Why the peer was banned.
        reason: BanCause,
    },
    /// A previously banned peer had its ban lifted.
    Unbanned {
        /// The peer's overlay address.
        overlay: OverlayAddress,
    },
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    // Compile-time check: both traits must stay object safe.
    fn _assert_object_safe(_: &dyn PeerReporter, _: &dyn AdmissionControl) {}

    #[derive(Default)]
    struct RecordingReporter {
        reports: Mutex<Vec<(OverlayAddress, SwarmScoringEvent, ReportSource)>>,
    }

    impl PeerReporter for RecordingReporter {
        fn report_peer(
            &self,
            overlay: &OverlayAddress,
            event: SwarmScoringEvent,
            source: ReportSource,
        ) {
            self.reports.lock().unwrap().push((*overlay, event, source));
        }
    }

    /// A ledger with a fixed headroom to both thresholds and a zero balance, so
    /// admission reduces to `price <= headroom`.
    struct FixedHeadroom(Au);

    impl Ledger for FixedHeadroom {
        fn balance(&self, _overlay: &OverlayAddress) -> Au {
            Au::ZERO
        }

        fn reserved(&self, _overlay: &OverlayAddress) -> Au {
            Au::ZERO
        }

        fn headroom(&self, _overlay: &OverlayAddress, _to: Threshold) -> Au {
            self.0
        }

        fn disconnect_line(&self, _overlay: &OverlayAddress) -> Au {
            self.0
        }

        fn settle_trigger(&self, _overlay: &OverlayAddress) -> Au {
            self.0
        }
    }

    #[test]
    fn report_peer_via_arc_auto_impl() {
        let reporter = Arc::new(RecordingReporter::default());
        let overlay = OverlayAddress::zero();

        fn report_all(reporter: impl PeerReporter, overlay: &OverlayAddress) {
            reporter.report_peer(
                overlay,
                SwarmScoringEvent::HandshakeFailure,
                ReportSource::Handshake,
            );
            reporter.report_peer(
                overlay,
                SwarmScoringEvent::RetrievalFailure,
                ReportSource::Protocol("retrieval"),
            );
        }
        report_all(Arc::clone(&reporter), &overlay);

        let reports = reporter.reports.lock().unwrap();
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].1, SwarmScoringEvent::HandshakeFailure);
        assert_eq!(reports[0].2, ReportSource::Handshake);
        assert_eq!(reports[1].2, ReportSource::Protocol("retrieval"));
    }

    #[test]
    fn admit_via_arc_auto_impl() {
        // `Ledger` auto-impls through `Arc`, and the blanket `AdmissionControl`
        // gives the boxed handle the band. A request at the headroom is admitted;
        // one over it is refused (payment headroom equals disconnect here, so the
        // band has no settle middle).
        let ledger: Arc<dyn AdmissionControl> = Arc::new(FixedHeadroom(Au::from_amount(100)));
        let overlay = OverlayAddress::zero();
        assert_eq!(
            ledger.admit(&overlay, Au::from_amount(100)),
            Admission::Admit
        );
        assert_eq!(
            ledger.admit(&overlay, Au::from_amount(101)),
            Admission::Refuse
        );
        assert_eq!(ledger.balance(&overlay), Au::ZERO);
    }

    #[test]
    fn scoring_event_default_weights() {
        assert!(SwarmScoringEvent::ConnectionSuccess { latency: None }.default_weight() > 0.0);
        assert!(
            SwarmScoringEvent::RetrievalSuccess {
                latency: Duration::ZERO
            }
            .default_weight()
                > 0.0
        );
        assert!(SwarmScoringEvent::GossipUseful.default_weight() > 0.0);

        assert!(SwarmScoringEvent::ConnectionTimeout.default_weight() < 0.0);
        assert!(SwarmScoringEvent::HandshakeFailure.default_weight() < 0.0);
        assert!(SwarmScoringEvent::MaliciousBehavior.default_weight() < -10.0);
    }

    #[test]
    fn scoring_event_latency_extraction() {
        let event = SwarmScoringEvent::ConnectionSuccess {
            latency: Some(Duration::from_millis(50)),
        };
        assert_eq!(event.latency(), Some(Duration::from_millis(50)));

        let event = SwarmScoringEvent::ConnectionTimeout;
        assert_eq!(event.latency(), None);
    }

    #[test]
    fn scoring_event_severity() {
        assert!(SwarmScoringEvent::MaliciousBehavior.is_severe());
        assert!(SwarmScoringEvent::InvalidData.is_severe());
        assert!(SwarmScoringEvent::AccountingViolation.is_severe());
        assert!(!SwarmScoringEvent::ConnectionTimeout.is_severe());
    }

    #[test]
    fn scoring_event_derives() {
        let event = SwarmScoringEvent::PingSuccess {
            latency: Duration::from_millis(5),
        };
        let copy = event;
        assert_eq!(event, copy);

        let label: &'static str = SwarmScoringEvent::GossipStale.into();
        assert_eq!(label, "gossip_stale");
    }

    #[test]
    fn cause_labels_are_snake_case() {
        let label: &'static str = DisconnectReason::LowScore.into();
        assert_eq!(label, "low_score");
        assert_eq!(
            DisconnectReason::AllowanceExceeded.to_string(),
            "allowance_exceeded"
        );

        let label: &'static str = BanCause::InvalidData.into();
        assert_eq!(label, "invalid_data");
    }

    #[test]
    fn only_remote_close_is_peer_attributable() {
        assert!(!DisconnectReason::RemoteClose.is_locally_initiated());
        for reason in [
            DisconnectReason::BinTrimmed,
            DisconnectReason::Banned,
            DisconnectReason::LowScore,
            DisconnectReason::BootnodeRotation,
            DisconnectReason::IdleTimeout,
            DisconnectReason::LocalClose,
            DisconnectReason::ShuttingDown,
        ] {
            assert!(reason.is_locally_initiated(), "{reason} must be local");
        }
    }

    #[test]
    fn lifecycle_event_is_cloneable() {
        let event = PeerLifecycleEvent::Banned {
            overlay: OverlayAddress::zero(),
            until: Some(1_750_000_000),
            reason: BanCause::LowScore,
        };
        let cloned = event.clone();
        assert!(matches!(
            cloned,
            PeerLifecycleEvent::Banned {
                reason: BanCause::LowScore,
                ..
            }
        ));
    }
}
