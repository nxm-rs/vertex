//! Peer reporting, affordability, and lifecycle event types.
//!
//! These types are the shared vocabulary between the peer manager and the
//! subsystems that observe or judge peers:
//!
//! - [`PeerReporter`] is the single sanctioned path for scoring input. The
//!   peer manager implements it via its handle; topology, gossip
//!   verification, handshake, protocol handlers, bandwidth accounting, and
//!   the RPC surface all report through it instead of mutating scores
//!   directly.
//! - [`PeerAffordability`] is implemented by bandwidth accounting so that
//!   protocol handlers can check whether a peer can pay for a request
//!   before doing the work.
//! - [`PeerLifecycleEvent`] is emitted by the peer manager; topology
//!   subscribes to it and executes the resulting disconnects and dial
//!   policy changes.

use core::time::Duration;

use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};

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
    /// Gossiped peer was verified via handshake (signature, overlay, multiaddr all match).
    GossipVerified,
    /// Gossiped peer failed verification (overlay, signature, or multiaddr mismatch).
    GossipInvalid,
    /// Gossiped peer could not be reached for verification.
    GossipUnreachable,
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
            Self::GossipVerified => 1.0,
            Self::GossipInvalid => -15.0,
            Self::GossipUnreachable => -0.5,
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
            Self::InvalidData
                | Self::MaliciousBehavior
                | Self::AccountingViolation
                | Self::GossipInvalid
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
    /// Hive gossip verification.
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

/// Query whether a peer can pay for service.
///
/// Implemented by bandwidth accounting; protocol handlers consult it before
/// serving a request so that work is never done for a peer that cannot
/// settle it. Prices and allowances are in accounting units (AU).
#[auto_impl::auto_impl(&, Arc)]
pub trait PeerAffordability: Send + Sync {
    /// True if the peer can afford a request of the given price in AU.
    fn can_afford(&self, overlay: &OverlayAddress, price: u64) -> bool;

    /// Remaining allowance for the peer in AU.
    fn allowance_remaining(&self, overlay: &OverlayAddress) -> u64;
}

/// Why a peer's connection should be closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum DisconnectCause {
    /// Score fell below the disconnect threshold.
    LowScore,
    /// A protocol handler reported a violation.
    ProtocolViolation,
    /// The peer exhausted its bandwidth allowance.
    AllowanceExceeded,
    /// Disconnect requested by an operator over the RPC surface.
    Requested,
    /// The connection slot was reclaimed by topology pruning.
    Pruned,
    /// The node is shutting down.
    ShuttingDown,
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
        reason: DisconnectCause,
    },
    /// The peer was banned.
    Banned {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// Unix timestamp in seconds at which the ban expires.
        until: u64,
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
    fn _assert_object_safe(_: &dyn PeerReporter, _: &dyn PeerAffordability) {}

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

    struct FixedAffordability(u64);

    impl PeerAffordability for FixedAffordability {
        fn can_afford(&self, _overlay: &OverlayAddress, price: u64) -> bool {
            price <= self.0
        }

        fn allowance_remaining(&self, _overlay: &OverlayAddress) -> u64 {
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
    fn affordability_via_arc_auto_impl() {
        let accounting: Arc<dyn PeerAffordability> = Arc::new(FixedAffordability(100));
        let overlay = OverlayAddress::zero();
        assert!(accounting.can_afford(&overlay, 100));
        assert!(!accounting.can_afford(&overlay, 101));
        assert_eq!(accounting.allowance_remaining(&overlay), 100);
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
        assert!(SwarmScoringEvent::GossipInvalid.is_severe());
        assert!(!SwarmScoringEvent::ConnectionTimeout.is_severe());
    }

    #[test]
    fn scoring_event_derives() {
        let event = SwarmScoringEvent::PingSuccess {
            latency: Duration::from_millis(5),
        };
        let copy = event;
        assert_eq!(event, copy);

        let label: &'static str = SwarmScoringEvent::GossipInvalid.into();
        assert_eq!(label, "gossip_invalid");
    }

    #[test]
    fn cause_labels_are_snake_case() {
        let label: &'static str = DisconnectCause::LowScore.into();
        assert_eq!(label, "low_score");
        assert_eq!(
            DisconnectCause::AllowanceExceeded.to_string(),
            "allowance_exceeded"
        );

        let label: &'static str = BanCause::InvalidData.into();
        assert_eq!(label, "invalid_data");
    }

    #[test]
    fn lifecycle_event_is_cloneable() {
        let event = PeerLifecycleEvent::Banned {
            overlay: OverlayAddress::zero(),
            until: 1_750_000_000,
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
