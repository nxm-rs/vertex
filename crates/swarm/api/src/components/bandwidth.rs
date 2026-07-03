//! Bandwidth accounting - per-peer balance tracking.
//!
//! Two-level design: [`SwarmAccounting`] creates per-peer [`SwarmPeerAccounting`] handles.
//! Uses overlay addresses for peer identification (not libp2p `PeerId`).
//!
//! # Settlement Providers
//!
//! Settlement is handled by pluggable [`SwarmSettlementProvider`] implementations:
//! - **Pseudosettle**: Time-based debt forgiveness (soft accounting)
//! - **Swap**: Chequebook-based real payments

use core::future::Future;
use std::vec::Vec;

use nectar_primitives::ChunkAddress;
use vertex_swarm_primitives::OverlayAddress;

use crate::{
    Admission, AdmissionControl, Au, SwarmIdentity, SwarmNodeType, SwarmPricing, SwarmResult,
};

/// Direction of data transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Uploading data (sending to peer)
    Upload,
    /// Downloading data (receiving from peer)
    Download,
}

/// Abstract peer balance state read by settlement providers.
///
/// Positive balance = peer owes us, negative = we owe peer. The peer address is
/// passed separately to `settle`.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmPeerState: Send + Sync {
    /// Get the current balance (positive = peer owes us).
    fn balance(&self) -> Au;
}

/// Settlement provider for bandwidth accounting.
///
/// `settle()` runs when explicit settlement is requested. Injected into the
/// accounting core as `Box<dyn SwarmSettlementProvider>`, so the trait stays
/// object-safe via `async_trait`.
#[async_trait::async_trait]
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait SwarmSettlementProvider: Send + Sync + 'static {
    /// Called when explicit settlement is requested.
    /// Returns the amount settled, or an error if settlement failed.
    async fn settle(&self, peer: OverlayAddress, state: &dyn SwarmPeerState) -> SwarmResult<Au>;

    /// Human-readable name for logging.
    fn name(&self) -> &'static str;
}

/// Configuration for bandwidth accounting.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmAccountingConfig: Send + Sync {
    /// Payment threshold in AU (triggers settlement when peer debt exceeds this).
    fn payment_threshold(&self) -> Au;

    /// Payment tolerance percent (disconnect threshold = payment_threshold * (100 + tolerance) / 100).
    fn payment_tolerance_percent(&self) -> u64;

    /// Refresh rate in AU per second (for pseudosettle).
    fn refresh_rate(&self) -> Au;

    /// Early payment trigger percent (for swap).
    fn early_payment_percent(&self) -> u64;

    /// Scaling factor for client-only nodes (divides thresholds).
    fn client_only_factor(&self) -> u64;

    /// The disconnect threshold in AU: the payment threshold plus the tolerance
    /// markup, saturating so an overlarge threshold or tolerance cannot wrap.
    fn disconnect_threshold(&self) -> Au {
        let percent = 100u64.saturating_add(self.payment_tolerance_percent());
        self.payment_threshold().scale_percent(percent)
    }

    /// The debt at which a debtor should settle early: the payment threshold less
    /// the early-payment headroom. The refresh-rate floor is applied by callers
    /// that need a minimum the peer will act on.
    fn early_payment_trigger(&self) -> Au {
        let early = self.early_payment_percent().min(100);
        self.payment_threshold()
            .checked_scale(100 - early)
            .map(|scaled| Au::from_amount(scaled.as_amount() / 100))
            .unwrap_or(Au::from_amount(u64::MAX))
    }

    /// The unscaled payment threshold the creditor serve lines derive from.
    ///
    /// Serve lines key on the remote's node type only; the debtor direction
    /// (self-pacing, the receive gate) keeps the own-type-scaled
    /// [`payment_threshold`](Self::payment_threshold). Implementations that
    /// scale the working threshold must preserve and return the base here.
    fn base_payment_threshold(&self) -> Au {
        self.payment_threshold()
    }

    /// The serve line for a client peer: the unscaled base threshold divided by
    /// the client-only factor, floored at one AU.
    fn client_payment_threshold(&self) -> Au {
        self.base_payment_threshold()
            .scale_down(self.client_only_factor())
            .max(Au::new(1))
    }

    /// The unscaled refresh rate the per-peer settlement allowance derives from.
    ///
    /// Allowance rates key on the remote's node type only; the debtor direction
    /// (our own settle pacing) keeps the own-type-scaled
    /// [`refresh_rate`](Self::refresh_rate). Implementations that scale the
    /// working rate must preserve and return the base here.
    fn base_refresh_rate(&self) -> Au {
        self.refresh_rate()
    }

    /// The settlement allowance rate for a client peer: the unscaled base rate
    /// divided by the client-only factor, floored at one AU.
    fn client_refresh_rate(&self) -> Au {
        self.base_refresh_rate()
            .scale_down(self.client_only_factor())
            .max(Au::new(1))
    }
}

/// A reserved receive leg awaiting commit or release.
///
/// `prepare_receive` returns a reservation that has already reserved balance.
/// Calling [`apply`](Self::apply) commits it (the balance change takes effect);
/// dropping it without applying releases the reservation. The receive leg is
/// committed by value the moment the chunk is in hand, so it needs no object-safe
/// path.
pub trait Commit: Send {
    /// Commit the reserved balance change.
    fn apply(self)
    where
        Self: Sized;
}

/// A reserved provide leg whose commit is deferred to a later wire write.
///
/// The forwarder hands an un-applied provide reservation to the wire-write site
/// as a `Box<dyn CommitOnWrite>` and commits it only once the bytes are on the
/// wire; dropping the box instead releases the reservation. Object-safe so
/// `ForwardedChunk`/`ForwardedReceipt` can hold it without naming the concrete
/// reservation type.
pub trait CommitOnWrite: Send {
    /// Commit the reserved balance change through a boxed reservation.
    fn apply_boxed(self: Box<Self>);

    /// Release without committing, recording that the peer refused the
    /// delivery: the answer was in hand and the write back to the peer
    /// failed. An accounting impl accrues this as ghost debt against the
    /// peer's serve headroom; the default releases like a plain drop. Never
    /// call it for a failure on our side of the relay.
    fn forfeit_boxed(self: Box<Self>) {}
}

/// The outcome of crediting one accepted inbound settlement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettlementCredit {
    /// The peer's cumulative accepted repayment after this credit.
    pub total: Au,
    /// The serve line as raised by a crossed growth checkpoint, `None` when no
    /// checkpoint was crossed. Reads back the enforced per-peer state, so an
    /// announcement built on it carries exactly what the provide gate enforces.
    pub raised_serve_line: Option<Au>,
}

/// Per-peer bandwidth accounting handle.
///
/// Reached through the [`SwarmAccounting::Peer`] associated type
/// (never as a trait object), so `settle` returns `impl Future + Send`
/// natively; the `Send` bound keeps settlement awaitable from spawned tasks.
pub trait SwarmPeerAccounting: Send + Sync {
    /// Record a priced amount of bandwidth usage (lock-free, must not block).
    fn record(&self, amount: Au, direction: Direction);

    /// Credit an accepted inbound settlement, accumulate it toward the peer's
    /// cumulative repayment, and evaluate the threshold-growth checkpoint in
    /// the same step.
    ///
    /// Settlement-method-agnostic: every provider's validated inbound credit
    /// lands here and feeds one accumulator and one checkpoint schedule. The
    /// caller clamps `amount` before calling; this method never validates.
    fn settlement_received(&self, amount: Au) -> SettlementCredit;

    /// The per-second time-based settlement allowance extended to this peer,
    /// keyed on its handshake node type at connect.
    fn refresh_allowance(&self) -> Au;

    /// Get current balance (positive = peer owes us).
    fn balance(&self) -> Au;

    /// Request settlement (may involve network I/O).
    fn settle(&self) -> impl Future<Output = SwarmResult<()>> + Send;

    /// Get the peer's overlay address.
    fn peer(&self) -> OverlayAddress;
}

/// Factory for creating per-peer bandwidth accounting handles.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmAccounting: Send + Sync {
    /// The node identity type.
    type Identity: SwarmIdentity;

    /// The per-peer accounting handle type.
    type Peer: SwarmPeerAccounting;

    /// Reservation for receiving service (balance decreases), committed by value.
    type ReceiveAction: Commit;

    /// Reservation for providing service (balance increases), committed on the
    /// deferred wire write.
    type ProvideAction: CommitOnWrite;

    /// Get the node's identity.
    fn identity(&self) -> &Self::Identity;

    /// Get or create an accounting handle for a peer.
    fn for_peer(&self, peer: OverlayAddress) -> Self::Peer;

    /// List all peers with active accounting.
    fn peers(&self) -> Vec<OverlayAddress>;

    /// Remove accounting for a peer.
    fn remove_peer(&self, peer: &OverlayAddress);

    /// Prepare to receive service from a peer (balance decreases).
    ///
    /// Returns an action that reserves balance. Call `apply()` to commit
    /// or drop to release the reservation.
    fn prepare_receive(
        &self,
        peer: OverlayAddress,
        price: Au,
        originated: bool,
    ) -> SwarmResult<Self::ReceiveAction>;

    /// Prepare to provide service to a peer (balance increases).
    fn prepare_provide(&self, peer: OverlayAddress, price: Au) -> SwarmResult<Self::ProvideAction>;

    /// Seed or update the peer's serve line from its handshake node type and
    /// return the line now in force. Storer peers get the full payment
    /// threshold, client peers the client-only-factor-scaled line; a peer never
    /// connected keeps the stricter client line. Callers invoke this at
    /// handshake completion and may announce the returned line to the peer, so
    /// it must be read back from the state the provide gate enforces.
    fn connect_peer(&self, peer: OverlayAddress, node_type: SwarmNodeType) -> Au;

    /// Adopt the peer-announced payment threshold as its settle line, clamped by
    /// the implementation. Inbound announcements only; never widens our serve line.
    fn adopt_payment_threshold(&self, peer: OverlayAddress, announced: Au) {
        let _ = (peer, announced);
    }
}

/// The object-safe origin dispatch gate over one accounting instance: the
/// admission band and the dispatch-committed debit read the same ledger
/// through one handle (the client service cannot name the `ReceiveAction`
/// type).
///
/// The origin commit point is at dispatch: an origin leg may be raced, and a
/// losing raced leg is cancelled by drop, so a commit deferred to delivery
/// could un-book debt for bytes the wire may still deliver. `debit_received`
/// therefore commits in one step and `refund_received` reverses it only when
/// the request provably reached no charge. An `Err` carries the
/// disconnect-threshold breach, a local pacing refusal that is not scored
/// against the peer.
pub trait OriginAccounting: Send + Sync {
    /// Band `price` against the peer's projected debt.
    fn admit(&self, peer: &OverlayAddress, price: Au) -> Admission;

    /// Debit `peer` by `price` for a received chunk, committing immediately.
    fn debit_received(&self, peer: OverlayAddress, price: Au, originated: bool) -> SwarmResult<()>;

    /// Credit back a receive debit committed at dispatch. Inverse of the
    /// dispatch commit: the balance moves back, the reservation is already
    /// cleared. Pure ledger op, never peer scoring.
    fn refund_received(&self, peer: OverlayAddress, price: Au);
}

impl<B: SwarmAccounting + AdmissionControl> OriginAccounting for B {
    fn admit(&self, peer: &OverlayAddress, price: Au) -> Admission {
        AdmissionControl::admit(self, peer, price)
    }

    fn debit_received(&self, peer: OverlayAddress, price: Au, originated: bool) -> SwarmResult<()> {
        self.prepare_receive(peer, price, originated)
            .map(Commit::apply)
    }

    fn refund_received(&self, peer: OverlayAddress, price: Au) {
        self.for_peer(peer).record(price, Direction::Upload);
    }
}

/// Combined pricing and bandwidth accounting for client operations.
///
/// Unifies chunk pricing and bandwidth accounting so callers don't need
/// to coordinate between separate pricer and accounting instances.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmClientAccounting: Send + Sync {
    /// The underlying bandwidth accounting type.
    type Accounting: SwarmAccounting;

    /// The pricing strategy type.
    type Pricing: SwarmPricing;

    /// Get the bandwidth accounting.
    fn accounting(&self) -> &Self::Accounting;

    /// Get the pricer.
    fn pricing(&self) -> &Self::Pricing;

    /// Prepare to receive a chunk (we pay, balance decreases).
    fn prepare_receive_chunk(
        &self,
        peer: OverlayAddress,
        chunk: &ChunkAddress,
        originated: bool,
    ) -> SwarmResult<<Self::Accounting as SwarmAccounting>::ReceiveAction> {
        let price = self.pricing().peer_price(&peer, chunk);
        self.accounting().prepare_receive(peer, price, originated)
    }

    /// Prepare to provide a chunk (peer pays, balance increases).
    fn prepare_provide_chunk(
        &self,
        peer: OverlayAddress,
        chunk: &ChunkAddress,
    ) -> SwarmResult<<Self::Accounting as SwarmAccounting>::ProvideAction> {
        let price = self.pricing().peer_price(&peer, chunk);
        self.accounting().prepare_provide(peer, price)
    }
}
