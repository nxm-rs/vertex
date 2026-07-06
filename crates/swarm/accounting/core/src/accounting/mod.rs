//! Per-peer bandwidth accounting.
//!
//! # Accounting Units (AU)
//!
//! All values are in **Accounting Units**, not bytes or BZZ tokens. AUs encode
//! network cost based on Kademlia proximity:
//!
//! ```text
//! price = (max_po - proximity + 1) × base_price
//! ```
//!
//! Closer chunks (higher proximity) cost less; distant chunks cost more.
//!
//! # Components
//!
//! - [`PeerState`] - Atomic per-peer balance counters
//! - [`Accounting`] - Factory with pluggable settlement providers
//! - [`Reservation`] - Typed receive/provide reservation legs

mod error;
mod peer;
mod reservation;

pub use error::AccountingError;
pub use peer::PeerState;
pub use reservation::{Provide, Receive, Reservation};

use parking_lot::RwLock;
use rustc_hash::FxBuildHasher;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use vertex_swarm_api::{
    Au, Debt, Direction, Ledger, LedgerSnapshot, SettlementCredit, SwarmAccounting,
    SwarmAccountingConfig, SwarmIdentity, SwarmNodeType, SwarmPeerAccounting, SwarmResult,
};
use vertex_swarm_primitives::OverlayAddress;

use vertex_swarm_api::SwarmSettlementProvider;

use crate::constants::{DEFAULT_MAX_INFLIGHT_GLOBAL, DEFAULT_MAX_INFLIGHT_PER_PEER};
use crate::persistence::{BalanceStore, BalanceStoreError, PersistedBalance};

/// The lower clamp on an adopted settle line, in refresh-rate units: a peer
/// cannot drive our settle timing tighter than twice the refresh rate.
const MIN_ANNOUNCED_REFRESH_MULTIPLES: u64 = 2;

/// Caps on outstanding reservations: `per_peer` for each leg, `global` across
/// all peers and both legs.
///
/// The threshold projections bound reserved AU per peer, but not the COUNT of
/// concurrent holds or their total across peers, so a flood of cheap in-flight
/// forwards could pin reservations and the node resources riding them without
/// crossing any threshold. The counts are released by the same drop that
/// releases the reserved balance.
#[derive(Clone, Copy, Debug)]
pub struct ReservationCaps {
    /// Outstanding reservations allowed per peer, per leg.
    pub per_peer: u64,
    /// Outstanding reservations allowed across all peers and both legs.
    pub global: u64,
}

impl Default for ReservationCaps {
    fn default() -> Self {
        Self {
            per_peer: DEFAULT_MAX_INFLIGHT_PER_PEER,
            global: DEFAULT_MAX_INFLIGHT_GLOBAL,
        }
    }
}

/// Per-peer accounting with pluggable settlement providers.
///
/// Manages balances and delegates settlement to configured providers.
/// Without providers, behaves as a simple balance tracker.
pub struct Accounting<C, I: SwarmIdentity> {
    config: C,
    identity: I,
    providers: Arc<[Box<dyn SwarmSettlementProvider>]>,
    // Overlay keys are uniformly random, so a fast non-DoS hasher is safe here
    // and removes SipHash from the per-candidate selection hot path.
    peers: RwLock<HashMap<OverlayAddress, Arc<PeerState>, FxBuildHasher>>,
    caps: ReservationCaps,
    /// Outstanding reservations across all peers and both legs, shared into
    /// every [`Reservation`] so the resolution drop is the single release point.
    inflight_global: Arc<AtomicU64>,
    /// Optional write-behind persistence for per-peer balances. `None` leaves the
    /// ledger purely in-memory (no node database, or the wasm client).
    store: Option<Arc<dyn BalanceStore>>,
}

impl<C: SwarmAccountingConfig, I: SwarmIdentity> Accounting<C, I> {
    /// Create a new accounting instance with no settlement providers.
    pub fn new(config: C, identity: I) -> Self {
        Self {
            config,
            identity,
            providers: Arc::from(Vec::new()),
            peers: RwLock::new(HashMap::default()),
            caps: ReservationCaps::default(),
            inflight_global: Arc::new(AtomicU64::new(0)),
            store: None,
        }
    }

    /// Create a new accounting instance with the given settlement providers.
    ///
    /// Providers are called in order during settlement operations; pseudosettle
    /// should come before swap.
    pub fn with_providers(
        config: C,
        identity: I,
        providers: Vec<Box<dyn SwarmSettlementProvider>>,
    ) -> Self {
        Self {
            config,
            identity,
            providers: Arc::from(providers),
            peers: RwLock::new(HashMap::default()),
            caps: ReservationCaps::default(),
            inflight_global: Arc::new(AtomicU64::new(0)),
            store: None,
        }
    }

    /// Override the outstanding-reservation caps.
    pub fn with_reservation_caps(mut self, caps: ReservationCaps) -> Self {
        self.caps = caps;
        self
    }

    /// Attach a write-behind balance store. `None` keeps the ledger in-memory.
    pub fn with_balance_store(mut self, store: Option<Arc<dyn BalanceStore>>) -> Self {
        self.store = store;
        self
    }

    /// Reload persisted balances into the ledger. Must run at construction,
    /// before any request is served, so a restarted debtor knows what it owes.
    ///
    /// Returns the number of peers restored. A store error is surfaced to the
    /// caller rather than panicking; the caller logs and continues on an
    /// empty ledger, the only safe degrade.
    pub fn restore(&self) -> Result<usize, BalanceStoreError> {
        let Some(store) = self.store.as_ref() else {
            return Ok(0);
        };
        let records = store.load()?;
        let restored = records.len();
        let mut peers = self.peers.write();
        for (peer, record) in records {
            peers
                .entry(peer)
                .or_insert_with(|| self.new_peer_state())
                .restore_balance(Au::new(record.balance));
        }
        Ok(restored)
    }

    /// Drain the dirty set and upsert those balances. Called on the persistence
    /// tick and on shutdown; a no-op without a store.
    ///
    /// The dirty flag is cleared before the balance is read (race-free with a
    /// concurrent apply), and re-armed if the write fails so the next tick
    /// retries rather than dropping the mark.
    pub fn flush(&self) -> Result<(), BalanceStoreError> {
        let Some(store) = self.store.as_ref() else {
            return Ok(());
        };
        // Collect the dirty peers under the read lock; the store write runs off
        // the lock. Each entry keeps its state so a failed write can re-arm it.
        let dirty: Vec<(OverlayAddress, Arc<PeerState>)> = {
            let peers = self.peers.read();
            peers
                .iter()
                .filter(|(_, state)| state.take_dirty())
                .map(|(peer, state)| (*peer, Arc::clone(state)))
                .collect()
        };
        if dirty.is_empty() {
            return Ok(());
        }
        let records: Vec<(OverlayAddress, PersistedBalance)> = dirty
            .iter()
            .map(|(peer, state)| {
                (
                    *peer,
                    PersistedBalance {
                        balance: state.balance().get(),
                    },
                )
            })
            .collect();
        if let Err(e) = store.flush(&records) {
            for (_, state) in &dirty {
                state.mark_dirty();
            }
            return Err(e);
        }
        Ok(())
    }

    /// Seed a fresh peer state at the config-derived default lines. The serve
    /// line defaults to the stricter client line (healed by `connect_peer` once
    /// the node type is known) and the settle line to the full local payment
    /// threshold (tightened by a later announcement).
    fn new_peer_state(&self) -> Arc<PeerState> {
        Arc::new(PeerState::new(
            self.config.client_payment_threshold(),
            self.config.payment_threshold(),
            self.config.disconnect_threshold(),
            self.config.client_refresh_rate(),
        ))
    }

    /// Returns the names of the active settlement providers.
    pub fn provider_names(&self) -> Vec<&str> {
        self.providers.iter().map(|p| p.name()).collect()
    }

    /// Prepare a receive reservation (we are receiving service, balance decreases).
    ///
    /// The hard gate shares one boundary with the advisory
    /// [`AdmissionControl::admit`](vertex_swarm_api::AdmissionControl::admit):
    /// it calls `admit` and refuses a [`Refuse`](vertex_swarm_api::Admission::Refuse)
    /// band. The breach is never scored against the peer; our debt reaching our own
    /// disconnect line is a local pacing outcome, not peer misbehaviour, and the
    /// remote enforces its own view by refusing or resetting us.
    pub fn prepare_receive(
        &self,
        peer: OverlayAddress,
        price: Au,
        _originated: bool,
    ) -> Result<Reservation<Receive>, AccountingError> {
        let state = self.peer_state(peer);
        if !state_snapshot(&state, self.settle_trigger_for(&state))
            .admit(price)
            .admits()
        {
            return Err(AccountingError::DisconnectThreshold {
                peer,
                balance: state.balance(),
                threshold: self.config.disconnect_threshold(),
            });
        }

        // Absolute reserve bound: a positive balance widens the admission
        // projection, so without this cap a creditor peer could pin reserved
        // headroom well past the disconnect line. Check-then-add can overshoot
        // under concurrency by at most the racing prices; the bound is a soft
        // flood ceiling, not a ledger invariant.
        let reserved = state.reserved_balance();
        let cap = state.disconnect_threshold();
        if reserved.saturating_add(price) > cap {
            return Err(AccountingError::ReserveCap {
                peer,
                reserved,
                cap,
            });
        }
        self.acquire_inflight::<Receive>(peer, &state)?;
        state.add_reserved(price);
        Ok(Reservation::new(
            state,
            price,
            Arc::clone(&self.inflight_global),
        ))
    }

    /// Prepare a provide action (we are providing service, balance increases).
    ///
    /// Refuse to serve once the peer's projected debt to us (committed balance
    /// plus outstanding provides plus this price) would cross the per-peer
    /// payment threshold, the point at which the peer is expected to settle.
    /// Without this gate a peer could free-ride up to the disconnect threshold
    /// per episode; the gate restores serve headroom only once the peer settles.
    /// The receive side keeps its own disconnect-threshold guard in
    /// [`Accounting::prepare_receive`].
    pub fn prepare_provide(
        &self,
        peer: OverlayAddress,
        price: Au,
    ) -> Result<Reservation<Provide>, AccountingError> {
        let state = self.peer_state(peer);

        let serve_line = state.serve_line();
        // Sign-safe exposure: the debt the peer owes us once this provide
        // commits. Reasoning in `Debt` keeps the comparison sign-safe, mirroring
        // the receive gate.
        let exposure = Debt::exposure(
            state.balance(),
            state.shadow_reserved_balance(),
            state.ghost_balance(),
            price,
        );
        if exposure.exceeds(serve_line) {
            return Err(AccountingError::PaymentThreshold {
                peer,
                balance: exposure.into(),
                threshold: serve_line,
            });
        }

        // Absolute reserve bound: our own debt to the peer widens the
        // exposure, so without this cap a peer we owe could pin shadow
        // reservations well past its serve line.
        let reserved = state.shadow_reserved_balance();
        if reserved.saturating_add(price) > serve_line {
            return Err(AccountingError::ReserveCap {
                peer,
                reserved,
                cap: serve_line,
            });
        }
        self.acquire_inflight::<Provide>(peer, &state)?;
        state.add_shadow_reserved(price);
        Ok(Reservation::new(
            state,
            price,
            Arc::clone(&self.inflight_global),
        ))
    }

    /// Take one in-flight slot for the leg, or refuse at either cap; the
    /// matching release is the reservation's resolution drop.
    fn acquire_inflight<L: reservation::Leg>(
        &self,
        peer: OverlayAddress,
        state: &PeerState,
    ) -> Result<(), AccountingError> {
        if self.inflight_global.fetch_add(1, Ordering::Relaxed) >= self.caps.global {
            self.inflight_global.fetch_sub(1, Ordering::Relaxed);
            return Err(AccountingError::GlobalInflightCap {
                cap: self.caps.global,
            });
        }
        if L::inflight(state).fetch_add(1, Ordering::Relaxed) >= self.caps.per_peer {
            L::inflight(state).fetch_sub(1, Ordering::Relaxed);
            self.inflight_global.fetch_sub(1, Ordering::Relaxed);
            return Err(AccountingError::PeerInflightCap {
                peer,
                cap: self.caps.per_peer,
            });
        }
        Ok(())
    }

    /// Get or create peer state (double-checked locking).
    pub fn peer_state(&self, peer: OverlayAddress) -> Arc<PeerState> {
        // Fast path: read lock
        if let Some(state) = self.peers.read().get(&peer) {
            return Arc::clone(state);
        }

        // Slow path: write lock. connect_peer heals the serve line in place once
        // the handshake node type is known; a later announcement tightens the
        // settle line through adopt_settle_line.
        self.peers
            .write()
            .entry(peer)
            .or_insert_with(|| self.new_peer_state())
            .clone()
    }

    /// Serve line for a peer of the given handshake node type. Selection keys on
    /// the REMOTE's type from the unscaled base, regardless of our own type: a
    /// storer remote gets the full base line, a client remote the
    /// client-only-factor-scaled base line.
    fn serve_line(&self, node_type: SwarmNodeType) -> Au {
        match node_type {
            SwarmNodeType::Storer => self.config.base_payment_threshold(),
            SwarmNodeType::Client | SwarmNodeType::Bootnode => {
                self.config.client_payment_threshold()
            }
        }
    }

    /// Settlement allowance rate for a peer of the given handshake node type,
    /// keyed on the remote's type from the unscaled base like the serve line.
    fn allowance_rate(&self, node_type: SwarmNodeType) -> Au {
        match node_type {
            SwarmNodeType::Storer => self.config.base_refresh_rate(),
            SwarmNodeType::Client | SwarmNodeType::Bootnode => self.config.client_refresh_rate(),
        }
    }

    /// The early-payment trigger floored at one refresh-rate unit.
    fn settle_trigger(&self) -> Au {
        self.config
            .early_payment_trigger()
            .max(self.config.refresh_rate())
    }

    /// The per-peer settle trigger: the peer's settle line less the early-payment
    /// headroom, floored at one refresh-rate unit. For an un-announced peer the
    /// settle line equals the local payment threshold, so this is identical to
    /// [`Accounting::settle_trigger`].
    fn settle_trigger_for(&self, state: &PeerState) -> Au {
        let early = self.config.early_payment_percent().min(100);
        state
            .settle_line()
            .scale_percent(100 - early)
            .max(self.config.refresh_rate())
    }

    /// Adopt a peer's announced payment threshold as its settle line, clamped to
    /// `[2 * refresh_rate, local payment threshold]`. The lower bound floors an
    /// adversarial tiny or zero announcement; the local payment threshold caps it
    /// so adoption only ever tightens our settle timing, never widens it past the
    /// config default. Writes only the settle line, never the serve line.
    fn adopt_announced(&self, peer: OverlayAddress, announced: Au) {
        let min = self
            .config
            .refresh_rate()
            .checked_scale(MIN_ANNOUNCED_REFRESH_MULTIPLES)
            .unwrap_or(Au::new(i64::MAX));
        let max = self.config.payment_threshold();
        // max-then-min, not Ord::clamp: a misconfigured 2*refresh above the
        // threshold inverts the band, and std clamp panics on that; letting the
        // local ceiling win keeps the settle line at the config default instead.
        self.peer_state(peer)
            .set_settle_line(announced.max(min).min(max));
    }
}

/// Map peer state to the four admission fields a band reads.
fn state_snapshot(state: &PeerState, settle_trigger: Au) -> LedgerSnapshot {
    LedgerSnapshot {
        balance: state.balance(),
        reserved: state.reserved_balance(),
        disconnect_line: state.disconnect_threshold(),
        settle_trigger,
    }
}

impl<C: SwarmAccountingConfig, I: SwarmIdentity> SwarmAccounting for Accounting<C, I> {
    type Identity = I;
    type Peer = AccountingPeerHandle;
    type ReceiveAction = Reservation<Receive>;
    type ProvideAction = Reservation<Provide>;

    fn identity(&self) -> &I {
        &self.identity
    }

    fn for_peer(&self, peer: OverlayAddress) -> Self::Peer {
        let state = self.peer_state(peer);
        AccountingPeerHandle {
            peer,
            state,
            providers: Arc::clone(&self.providers),
            disconnect_threshold: self.config.disconnect_threshold(),
        }
    }

    fn peers(&self) -> Vec<OverlayAddress> {
        self.peers.read().keys().copied().collect()
    }

    fn remove_peer(&self, peer: &OverlayAddress) {
        self.peers.write().remove(peer);
    }

    fn prepare_receive(
        &self,
        peer: OverlayAddress,
        price: Au,
        originated: bool,
    ) -> SwarmResult<Reservation<Receive>> {
        Ok(Accounting::prepare_receive(self, peer, price, originated)?)
    }

    fn prepare_provide(
        &self,
        peer: OverlayAddress,
        price: Au,
    ) -> SwarmResult<Reservation<Provide>> {
        Ok(Accounting::prepare_provide(self, peer, price)?)
    }

    fn connect_peer(&self, peer: OverlayAddress, node_type: SwarmNodeType) -> Au {
        // Mutate the serve line on the shared PeerState in place. Replacing the
        // Arc would drop outstanding reservations that hold clones. The line is
        // read back from the state so an announcement built on the return value
        // carries exactly what the provide gate enforces. Each connection also
        // restarts threshold growth: the serve line returns to the node-type
        // default and repayment toward the next raise is earned afresh.
        let state = self.peer_state(peer);
        let rate = self.allowance_rate(node_type);
        state.set_serve_line(self.serve_line(node_type));
        state.set_allowance_rate(rate);
        state.reset_settlement_growth(rate);
        state.serve_line()
    }

    fn adopt_settle_line(&self, peer: OverlayAddress, announced: Au) {
        // Single-writer on the settle line, disjoint from connect_peer's serve
        // line, so the two compose with no write-write race on shared state.
        self.adopt_announced(peer, announced);
    }
}

/// Per-peer ledger reads for admission.
///
/// Sign convention: `balance` is the peer's debt to us in AU (positive means the
/// peer owes us, negative we owe the peer). The admission band that consumes
/// these reads lives in the default
/// [`AdmissionControl::admit`](vertex_swarm_api::AdmissionControl::admit). Unknown peers
/// read as fresh zero-balance peers with the configured thresholds, matching
/// [`Accounting::peer_state`], and the reads never insert peer state.
impl<C: SwarmAccountingConfig, I: SwarmIdentity> Ledger for Accounting<C, I> {
    fn balance(&self, peer: &OverlayAddress) -> Au {
        self.peers
            .read()
            .get(peer)
            .map_or(Au::ZERO, |state| state.balance())
    }

    fn reserved(&self, peer: &OverlayAddress) -> Au {
        self.peers
            .read()
            .get(peer)
            .map_or(Au::ZERO, |state| state.reserved_balance())
    }

    fn disconnect_line(&self, peer: &OverlayAddress) -> Au {
        self.peers.read().get(peer).map_or_else(
            || self.config.disconnect_threshold(),
            |state| state.disconnect_threshold(),
        )
    }

    fn settle_trigger(&self, peer: &OverlayAddress) -> Au {
        // The per-peer settle trigger derived from the peer's adopted settle line,
        // floored at one refresh-rate unit so a settle always offers at least the
        // minimum the peer acts on. An unknown peer (no adoption yet) falls back to
        // the config-derived trigger, matching the lazy peer_state seed, and never
        // inserts state.
        self.peers.read().get(peer).map_or_else(
            || Accounting::settle_trigger(self),
            |state| self.settle_trigger_for(state),
        )
    }

    fn snapshot(&self, peer: &OverlayAddress) -> LedgerSnapshot {
        // One read lock and one key hash for the per-peer fields. The fallback for
        // an unknown peer matches the per-field reads (fresh zero-balance peer at
        // the configured disconnect threshold and config-derived settle trigger),
        // so a band over this snapshot is identical to one over the separate reads.
        self.peers.read().get(peer).map_or_else(
            || LedgerSnapshot {
                balance: Au::ZERO,
                reserved: Au::ZERO,
                disconnect_line: self.config.disconnect_threshold(),
                settle_trigger: self.settle_trigger(),
            },
            |state| state_snapshot(state, self.settle_trigger_for(state)),
        )
    }
}

/// Handle to a peer's accounting state. Cheap to clone.
#[derive(Clone)]
pub struct AccountingPeerHandle {
    peer: OverlayAddress,
    state: Arc<PeerState>,
    providers: Arc<[Box<dyn SwarmSettlementProvider>]>,
    disconnect_threshold: Au,
}

impl AccountingPeerHandle {
    /// Get access to the underlying peer state.
    pub fn state(&self) -> &Arc<PeerState> {
        &self.state
    }

    /// Get the disconnect threshold in AU.
    pub fn disconnect_threshold(&self) -> Au {
        self.disconnect_threshold
    }

    /// Call `settle()` on providers in order until debt is below threshold.
    async fn settle_all(&self) -> SwarmResult<Au> {
        let mut total = Au::ZERO;

        for provider in self.providers.iter() {
            total = total.saturating_add(provider.settle(self.peer, self.state.as_ref()).await?);

            // Stop once the committed debt no longer exceeds the settle line.
            // Read live: an announcement may land between handle creation and this
            // break. Reasoning in `Debt` keeps the comparison sign-safe (both sides
            // non-negative); each provider re-reads `balance()` internally, so the
            // fresh committed debt drives the break.
            if !Debt::committed(self.state.balance()).exceeds(self.state.settle_line()) {
                break;
            }
        }

        Ok(total)
    }
}

impl SwarmPeerAccounting for AccountingPeerHandle {
    fn record(&self, amount: Au, direction: Direction) {
        match direction {
            Direction::Upload => self.state.add_balance(amount),
            Direction::Download => self.state.add_balance(-amount),
        }
    }

    fn settlement_received(&self, amount: Au) -> SettlementCredit {
        self.state.add_balance(-amount);
        let cumulative_repayment = self.state.add_repayment(amount);
        let raised_serve_line = self
            .state
            .grow_serve_line(cumulative_repayment, self.state.allowance_rate());
        SettlementCredit {
            cumulative_repayment,
            raised_serve_line,
        }
    }

    fn allowance_rate(&self) -> Au {
        self.state.allowance_rate()
    }

    fn balance(&self) -> Au {
        self.state.balance()
    }

    async fn settle(&self) -> SwarmResult<()> {
        self.settle_all().await.map(|_| ())
    }

    fn peer(&self) -> OverlayAddress {
        self.peer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccountingConfig, NoSettlement};
    use vertex_swarm_api::{Admission, AdmissionControl};
    use vertex_swarm_test_utils::{Identity, test_identity, test_peer};

    fn test_accounting() -> Accounting<AccountingConfig, Identity> {
        Accounting::new(AccountingConfig::default(), test_identity())
    }

    fn au(value: i64) -> Au {
        Au::new(value)
    }

    #[test]
    fn test_accounting_basic() {
        let accounting = test_accounting();

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), au(0));

        handle.record(au(1000), Direction::Upload);
        assert_eq!(handle.balance(), au(1000));

        handle.record(au(500), Direction::Download);
        assert_eq!(handle.balance(), au(500));
    }

    #[test]
    fn test_prepare_receive() {
        let accounting = test_accounting();

        let action = accounting
            .prepare_receive(test_peer(), au(1000), true)
            .expect("should prepare receive");

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.state.reserved_balance(), au(1000));

        action.apply();

        assert_eq!(handle.balance(), au(-1000));
        assert_eq!(handle.state.reserved_balance(), au(0));
    }

    #[test]
    fn test_refund_received_credits_back_a_committed_receive() {
        // The inverse of the dispatch commit: commit a receive debit, then refund
        // it and watch the balance return to zero. Reserved stays cleared.
        use vertex_swarm_api::OriginAccounting;

        let accounting = test_accounting();

        accounting
            .prepare_receive(test_peer(), au(1000), true)
            .expect("within threshold")
            .apply();

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), au(-1000));
        assert_eq!(handle.state.reserved_balance(), au(0));

        accounting.refund_received(test_peer(), au(1000));

        assert_eq!(handle.balance(), au(0));
        assert_eq!(handle.state.reserved_balance(), au(0));
    }

    #[test]
    fn test_prepare_receive_dropped() {
        let accounting = test_accounting();

        {
            let _action = accounting
                .prepare_receive(test_peer(), au(1000), true)
                .expect("should prepare receive");
        }

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), au(0));
        assert_eq!(handle.state.reserved_balance(), au(0));
    }

    #[test]
    fn test_with_single_provider() {
        let accounting = Accounting::with_providers(
            AccountingConfig::default(),
            test_identity(),
            vec![Box::new(NoSettlement)],
        );

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), au(0));

        handle.record(au(1000), Direction::Download);
        assert_eq!(handle.balance(), au(-1000));
    }

    #[test]
    fn test_with_two_providers() {
        let accounting = Accounting::with_providers(
            AccountingConfig::default(),
            test_identity(),
            vec![Box::new(NoSettlement), Box::new(NoSettlement)],
        );

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), au(0));

        handle.record(au(1000), Direction::Upload);
        assert_eq!(handle.balance(), au(1000));
    }

    #[test]
    fn test_peers_list() {
        let accounting = test_accounting();

        let peer1 = OverlayAddress::from([1u8; 32]);
        let peer2 = OverlayAddress::from([2u8; 32]);

        let _ = accounting.for_peer(peer1);
        let _ = accounting.for_peer(peer2);

        let peers = accounting.peers();
        assert_eq!(peers.len(), 2);
        assert!(peers.contains(&peer1));
        assert!(peers.contains(&peer2));
    }

    #[test]
    fn test_remove_peer() {
        let accounting = test_accounting();

        let peer = test_peer();
        let _ = accounting.for_peer(peer);

        assert_eq!(accounting.peers().len(), 1);

        accounting.remove_peer(&peer);

        assert_eq!(accounting.peers().len(), 0);
    }

    #[test]
    fn test_handle_clone() {
        let accounting = test_accounting();

        let handle1 = accounting.for_peer(test_peer());
        let handle2 = handle1.clone();

        handle1.record(au(1000), Direction::Upload);

        // Both handles should see the same balance (shared state)
        assert_eq!(handle1.balance(), au(1000));
        assert_eq!(handle2.balance(), au(1000));
    }

    /// Config with payment threshold 1000 and 25% tolerance, so the
    /// disconnect threshold is 1250.
    fn small_config() -> AccountingConfig {
        AccountingConfig::new(1000, 25, 0, 0, 5, crate::FixedPricingConfig::default())
    }

    const SMALL_DISCONNECT_THRESHOLD: Au = Au::new(1250);

    /// The default storer config scaled to the line a storer enforces on a
    /// client: payment 1_350_000, disconnect 1_687_500, settle trigger 675_000.
    fn client_config() -> AccountingConfig {
        AccountingConfig::default().for_client()
    }

    #[test]
    fn test_receive_breach_refuses_without_scoring_the_peer() {
        // A debtor breaching its own disconnect line refuses the receive so the
        // caller routes elsewhere, but never scores the creditor: the debt
        // reaching our own line is a local pacing outcome, not peer
        // misbehaviour. Accounting holds no reporter, so a breach can never feed
        // peer scoring.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        assert!(matches!(
            accounting.prepare_receive(peer, au(2000), true),
            Err(AccountingError::DisconnectThreshold { .. })
        ));

        // Retrying against the broken state keeps refusing.
        assert!(accounting.prepare_receive(peer, au(2000), true).is_err());
        assert!(accounting.prepare_receive(peer, au(3000), true).is_err());

        // A receive within the line is granted again, then a fresh breach
        // refuses once more.
        let action = accounting
            .prepare_receive(peer, au(100), true)
            .expect("within threshold");
        drop(action);
        assert!(accounting.prepare_receive(peer, au(2000), true).is_err());
    }

    #[test]
    fn test_no_reporter_behaviour_unchanged() {
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        assert!(matches!(
            accounting.prepare_receive(peer, au(2000), true),
            Err(AccountingError::DisconnectThreshold { .. })
        ));

        let action = accounting
            .prepare_receive(peer, SMALL_DISCONNECT_THRESHOLD, true)
            .expect("exactly at threshold is allowed");
        action.apply();

        let handle = accounting.for_peer(peer);
        assert_eq!(handle.balance(), -SMALL_DISCONNECT_THRESHOLD);
    }

    #[test]
    fn test_admit_unknown_peer_is_fresh_and_read_only() {
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        // Unknown peers are treated as fresh zero-balance peers, so the band
        // admits right up to the disconnect threshold.
        assert!(accounting.admit(&peer, SMALL_DISCONNECT_THRESHOLD).admits());
        assert!(
            !accounting
                .admit(&peer, SMALL_DISCONNECT_THRESHOLD + Au::new(1))
                .admits()
        );

        // Ledger reads never insert peer state.
        assert!(accounting.peers().is_empty());
    }

    #[test]
    fn test_admit_boundary_is_the_prepare_receive_boundary() {
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        // Build up debt: we owe the peer 500 AU.
        let handle = accounting.for_peer(peer);
        handle.record(au(500), Direction::Download);
        assert_eq!(handle.balance(), au(-500));

        // Exactly at the threshold: admitted and grantable (prepare_receive
        // routes through the same admit boundary).
        assert!(accounting.admit(&peer, au(750)).admits());
        assert!(accounting.prepare_receive(peer, au(750), true).is_ok());

        // Just over: refused by both, because they are one boundary.
        assert!(!accounting.admit(&peer, au(751)).admits());
        assert!(accounting.prepare_receive(peer, au(751), true).is_err());
    }

    #[test]
    fn test_admit_accounts_for_reservations() {
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        let action = accounting
            .prepare_receive(peer, au(1000), true)
            .expect("within threshold");

        // The outstanding reservation narrows the band: only 250 AU of the 1250
        // disconnect threshold remains admissible.
        assert!(accounting.admit(&peer, au(250)).admits());
        assert!(!accounting.admit(&peer, au(251)).admits());

        // Releasing the reservation restores the full band.
        drop(action);
        assert!(accounting.admit(&peer, SMALL_DISCONNECT_THRESHOLD).admits());
    }

    #[test]
    fn test_provide_refused_past_payment_threshold_until_settled() {
        // Payment threshold 1000, disconnect 1250.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();
        accounting.connect_peer(peer, SwarmNodeType::Storer);
        let handle = accounting.for_peer(peer);

        // Serving up to the payment threshold is allowed.
        let provide = accounting
            .prepare_provide(peer, au(1000))
            .expect("at payment threshold is allowed");
        provide.apply();
        assert_eq!(handle.balance(), au(1000));

        // The peer now owes us exactly the payment threshold. Any further
        // service is refused: it would push the projected debt over the
        // threshold, even though the disconnect threshold (1250) has not been
        // reached.
        assert!(matches!(
            accounting.prepare_provide(peer, au(1)),
            Err(AccountingError::PaymentThreshold { .. })
        ));

        // The peer settles (we forgive/receive its debt), restoring headroom.
        handle.record(au(600), Direction::Download);
        assert_eq!(handle.balance(), au(400));

        // Service resumes within the recovered headroom.
        let provide = accounting
            .prepare_provide(peer, au(600))
            .expect("settled peer is served again");
        provide.apply();
        assert_eq!(handle.balance(), au(1000));
    }

    #[test]
    fn test_provide_refusal_counts_outstanding_reservations() {
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();
        accounting.connect_peer(peer, SwarmNodeType::Storer);

        // An outstanding (un-applied) provide reserves shadow balance, so a
        // second provide that together crosses the threshold is refused.
        let _outstanding = accounting
            .prepare_provide(peer, au(900))
            .expect("first provide reserved");
        assert!(matches!(
            accounting.prepare_provide(peer, au(200)),
            Err(AccountingError::PaymentThreshold { .. })
        ));
        // A smaller provide that stays under the threshold still succeeds.
        assert!(accounting.prepare_provide(peer, au(100)).is_ok());
    }

    #[test]
    fn test_provide_negative_balance_widens_serve_headroom() {
        // Payment threshold 1000. A peer we already owe (negative balance) can be
        // served past the raw threshold: the debt we owe extends the headroom.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();
        accounting.connect_peer(peer, SwarmNodeType::Storer);
        let handle = accounting.for_peer(peer);

        // Download 500 from the peer, driving our balance to -500 (we owe them).
        handle.record(au(500), Direction::Download);
        assert_eq!(handle.balance(), au(-500));

        // The widening applies to committed balance: served in steps, the peer
        // is provided 1500 in total, past the raw 1000 line, with the exposure
        // ending exactly at the threshold. A single 1500 reservation would be
        // refused by the absolute reserve bound instead.
        accounting
            .prepare_provide(peer, au(1000))
            .expect("exposure 500 is within the line")
            .apply();
        assert_eq!(handle.balance(), au(500));
        accounting
            .prepare_provide(peer, au(500))
            .expect("exposure lands exactly at the line")
            .apply();
        assert_eq!(handle.balance(), au(1000));

        // One unit more crosses the threshold and refuses.
        assert!(matches!(
            accounting.prepare_provide(peer, au(1)),
            Err(AccountingError::PaymentThreshold { .. })
        ));
    }

    #[test]
    fn connect_peer_storer_gets_the_full_serve_line() {
        // small_config: storer line 1000, client line 200.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();
        let line = accounting.connect_peer(peer, SwarmNodeType::Storer);

        // The returned line is the state the provide gate enforces.
        assert_eq!(line, au(1000));
        assert!(accounting.prepare_provide(peer, au(1000)).is_ok());
        assert!(matches!(
            accounting.prepare_provide(peer, au(1001)),
            Err(AccountingError::PaymentThreshold { .. })
        ));
    }

    #[test]
    fn connect_peer_client_gets_the_scaled_serve_line() {
        // small_config: client line 1000 / factor 5 = 200.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();
        let line = accounting.connect_peer(peer, SwarmNodeType::Client);

        assert_eq!(line, au(200));
        assert!(accounting.prepare_provide(peer, au(200)).is_ok());
        assert!(matches!(
            accounting.prepare_provide(peer, au(201)),
            Err(AccountingError::PaymentThreshold { .. })
        ));
    }

    #[test]
    fn storer_and_client_peers_get_different_serve_lines_from_one_node() {
        // The anti-divergence invariant: one node keys the serve line on the
        // REMOTE's handshake type, never our own. A storer remote is served the
        // full line; a client remote only the scaled line, at the same time.
        let accounting = Accounting::new(small_config(), test_identity());
        let storer = OverlayAddress::from([1u8; 32]);
        let client = OverlayAddress::from([2u8; 32]);

        accounting.connect_peer(storer, SwarmNodeType::Storer);
        accounting.connect_peer(client, SwarmNodeType::Client);

        // The storer remote serves up to the full line.
        assert!(accounting.prepare_provide(storer, au(1000)).is_ok());
        // The client remote refuses past the scaled line but serves up to it.
        assert!(matches!(
            accounting.prepare_provide(client, au(201)),
            Err(AccountingError::PaymentThreshold { .. })
        ));
        assert!(accounting.prepare_provide(client, au(200)).is_ok());
    }

    #[test]
    fn unknown_peer_defaults_to_the_client_serve_line() {
        // A peer never connected (no handshake node type yet) is served on the
        // stricter client line, the load-bearing fallback for the dispatch race.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        assert!(matches!(
            accounting.prepare_provide(peer, au(201)),
            Err(AccountingError::PaymentThreshold { .. })
        ));
        assert!(accounting.prepare_provide(peer, au(200)).is_ok());
    }

    #[test]
    fn connect_peer_raises_a_lazily_created_peer_to_the_storer_line() {
        // A dispatch task creates the peer lazily on the client line; a later
        // connect_peer heals the same PeerState in place, without swapping the
        // Arc, so the serve line rises to the storer line.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        // Force lazy creation at the client line: au(1000) is refused.
        assert!(accounting.prepare_provide(peer, au(1000)).is_err());

        accounting.connect_peer(peer, SwarmNodeType::Storer);

        // The same peer is now served the full line.
        assert!(accounting.prepare_provide(peer, au(1000)).is_ok());
    }

    #[test]
    fn connect_peer_does_not_touch_the_debtor_disconnect_line() {
        // connect_peer re-keys only the creditor serve line. The debtor-side
        // receive gate (the disconnect line) is fixed at creation and unmoved by
        // either node type.
        let accounting = Accounting::new(small_config(), test_identity());
        let storer = OverlayAddress::from([3u8; 32]);
        accounting.connect_peer(storer, SwarmNodeType::Storer);

        // The receive boundary is still the disconnect line (1250), not the
        // serve line.
        assert!(
            accounting
                .prepare_receive(storer, SMALL_DISCONNECT_THRESHOLD, true)
                .is_ok()
        );
        assert!(matches!(
            accounting.prepare_receive(storer, SMALL_DISCONNECT_THRESHOLD + Au::new(1), true),
            Err(AccountingError::DisconnectThreshold { .. })
        ));

        // A client remote leaves the receive boundary at the disconnect line too.
        let client = OverlayAddress::from([4u8; 32]);
        accounting.connect_peer(client, SwarmNodeType::Client);
        assert!(
            accounting
                .prepare_receive(client, SMALL_DISCONNECT_THRESHOLD, true)
                .is_ok()
        );
        assert!(matches!(
            accounting.prepare_receive(client, SMALL_DISCONNECT_THRESHOLD + Au::new(1), true),
            Err(AccountingError::DisconnectThreshold { .. })
        ));
    }

    #[test]
    fn serve_lines_on_a_client_node_derive_from_the_unscaled_base() {
        // for_client scales only the debtor direction. The creditor serve lines
        // keep deriving from the unscaled base, keyed on the remote's type, and
        // clear the minimum an announced line must meet (twice the recipient's
        // refresh rate) for both remote types.
        use crate::constants::{
            DEFAULT_CLIENT_ONLY_FACTOR, DEFAULT_PAYMENT_THRESHOLD, DEFAULT_REFRESH_RATE,
        };

        let config = client_config();
        // Debtor direction stays scaled: self-pacing and the receive gate.
        assert_eq!(
            config.payment_threshold(),
            Au::from_amount(DEFAULT_PAYMENT_THRESHOLD / DEFAULT_CLIENT_ONLY_FACTOR)
        );
        assert!(config.disconnect_threshold() < Au::from_amount(DEFAULT_PAYMENT_THRESHOLD));

        let accounting = Accounting::new(config, test_identity());
        let storer_line = accounting.serve_line(SwarmNodeType::Storer);
        let client_line = accounting.serve_line(SwarmNodeType::Client);

        assert_eq!(storer_line, Au::from_amount(DEFAULT_PAYMENT_THRESHOLD));
        assert_eq!(
            client_line,
            Au::from_amount(DEFAULT_PAYMENT_THRESHOLD / DEFAULT_CLIENT_ONLY_FACTOR)
        );

        let full_minimum = Au::from_amount(2 * DEFAULT_REFRESH_RATE);
        let client_minimum =
            Au::from_amount(2 * (DEFAULT_REFRESH_RATE / DEFAULT_CLIENT_ONLY_FACTOR));
        assert!(storer_line >= full_minimum);
        assert!(client_line >= client_minimum);

        // connect_peer reports the same lines through the public seam, so the
        // announced values on a stock client node clear the minimums too.
        assert_eq!(
            accounting.connect_peer(OverlayAddress::from([3u8; 32]), SwarmNodeType::Storer),
            storer_line
        );
        assert_eq!(
            accounting.connect_peer(OverlayAddress::from([4u8; 32]), SwarmNodeType::Client),
            client_line
        );
    }

    #[test]
    fn test_admit_bands_settle_between_payment_and_disconnect() {
        // Payment 1000, disconnect 1250. A request landing the projected debt in
        // (payment, disconnect] is SettleAndAdmit; below is Admit; above Refuse.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        assert_eq!(accounting.admit(&peer, au(1000)), Admission::Admit);
        assert_eq!(accounting.admit(&peer, au(1001)), Admission::SettleAndAdmit);
        assert_eq!(accounting.admit(&peer, au(1250)), Admission::SettleAndAdmit);
        assert_eq!(accounting.admit(&peer, au(1251)), Admission::Refuse);
    }

    /// Reports settling a fixed amount, modelling a provider that pays only part
    /// of a large debt. In production the tracked balance is reduced by the
    /// async service ack, not by the provider, so the mock leaves the state arg
    /// untouched.
    struct PartialSettleProvider(Au);

    #[async_trait::async_trait]
    impl SwarmSettlementProvider for PartialSettleProvider {
        async fn settle(
            &self,
            _peer: OverlayAddress,
            _state: &dyn vertex_swarm_api::SwarmPeerState,
        ) -> SwarmResult<Au> {
            Ok(self.0)
        }

        fn name(&self) -> &'static str {
            "partial-settle"
        }
    }

    /// Records whether its `settle` ran.
    struct RecordingProvider(Arc<std::sync::atomic::AtomicBool>);

    #[async_trait::async_trait]
    impl SwarmSettlementProvider for RecordingProvider {
        async fn settle(
            &self,
            _peer: OverlayAddress,
            _state: &dyn vertex_swarm_api::SwarmPeerState,
        ) -> SwarmResult<Au> {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(Au::ZERO)
        }

        fn name(&self) -> &'static str {
            "recording"
        }
    }

    #[tokio::test]
    async fn settle_all_reaches_every_provider_while_debt_remains() {
        // Payment threshold 1000. A 5000 debt stays past the threshold while the
        // first provider settles only part of it, so the fan-out must run the
        // second provider too.
        let ran_second = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let accounting = Accounting::with_providers(
            small_config(),
            test_identity(),
            vec![
                Box::new(PartialSettleProvider(au(1000))),
                Box::new(RecordingProvider(Arc::clone(&ran_second))),
            ],
        );

        let handle = accounting.for_peer(test_peer());
        handle.record(au(5000), Direction::Download);
        assert_eq!(handle.balance(), au(-5000));

        handle.settle().await.expect("settle succeeds");

        assert!(
            ran_second.load(std::sync::atomic::Ordering::SeqCst),
            "the second provider must run while debt remains past the threshold"
        );
    }

    #[test]
    fn admit_settles_once_the_request_crosses_the_payment_threshold() {
        // Payment 1000, disconnect 1250. A fresh request that lands the projected
        // debt past the payment threshold settles; one that stays below does not.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        // We already owe 900; a 50 AU request stays under the 1000 payment line.
        let handle = accounting.for_peer(peer);
        handle.record(au(900), Direction::Download);
        assert!(!accounting.admit(&peer, au(50)).settles());

        // A 200 AU request lands the projected debt at 1100, past the payment
        // line but below disconnect: settle.
        assert!(accounting.admit(&peer, au(200)).settles());
    }

    #[test]
    fn admit_at_zero_price_settles_once_committed_debt_passes_the_settle_trigger() {
        // The client settle path calls `admit(peer, Au::ZERO).settles()`. With our
        // debt already past the payment threshold (1_350_000) but below the
        // disconnect line, a zero-price band must still settle. The earlier
        // floored-headroom reconstruction collapsed to `price > 0` here and stopped
        // settling exactly when the debt most needed paying down.
        let accounting = Accounting::new(client_config(), test_identity());
        let peer = test_peer();

        let handle = accounting.for_peer(peer);
        handle.record(au(1_500_000), Direction::Download);
        assert_eq!(handle.balance(), au(-1_500_000));

        assert!(
            accounting.admit(&peer, Au::ZERO).settles(),
            "a client over its payment threshold must still settle at zero price"
        );
    }

    #[test]
    fn admit_refuse_boundary_is_the_prepare_receive_boundary_at_the_disconnect_line() {
        // Payment 1000, disconnect 1250. The refuse band is the original
        // prepare_receive boundary: refuse exactly when the projected debt crosses
        // the disconnect line, and `prepare_receive` errors at the same point.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        // Fresh peer: the projected debt equals the price. At the disconnect line
        // the request is still admitted; one unit past it is refused.
        assert_ne!(accounting.admit(&peer, au(1250)), Admission::Refuse);
        assert_eq!(accounting.admit(&peer, au(1251)), Admission::Refuse);

        assert!(accounting.prepare_receive(peer, au(1250), true).is_ok());
        assert!(matches!(
            accounting.prepare_receive(peer, au(1251), true),
            Err(AccountingError::DisconnectThreshold { .. })
        ));
    }

    #[test]
    fn admit_settles_at_the_early_payment_trigger_not_the_payment_threshold() {
        // Payment 1000, disconnect 1250, early-payment 40% so the settle trigger is
        // 600, strictly below the payment threshold. A projected debt below 600
        // admits; at or above (and below disconnect) settles. This pins the settle
        // point to the early-payment value, not the full payment threshold.
        let config =
            AccountingConfig::new(1000, 25, 10, 40, 5, crate::FixedPricingConfig::default());
        let accounting = Accounting::new(config, test_identity());
        let peer = test_peer();

        assert_eq!(accounting.admit(&peer, au(600)), Admission::Admit);
        assert_eq!(accounting.admit(&peer, au(601)), Admission::SettleAndAdmit);
        assert_eq!(accounting.admit(&peer, au(1250)), Admission::SettleAndAdmit);
        assert_eq!(accounting.admit(&peer, au(1251)), Admission::Refuse);
    }

    #[test]
    fn receive_inflight_cap_refuses_until_a_reservation_resolves() {
        let accounting = test_accounting().with_reservation_caps(ReservationCaps {
            per_peer: 2,
            global: 100,
        });
        let peer = test_peer();

        let held = accounting
            .prepare_receive(peer, au(10), true)
            .expect("first slot");
        let second = accounting
            .prepare_receive(peer, au(10), true)
            .expect("second slot");
        assert!(matches!(
            accounting.prepare_receive(peer, au(10), true),
            Err(AccountingError::PeerInflightCap { .. })
        ));

        // A dropped reservation frees its slot; so does an applied one.
        drop(held);
        let third = accounting
            .prepare_receive(peer, au(10), true)
            .expect("slot freed by the drop");
        second.apply();
        assert!(accounting.prepare_receive(peer, au(10), true).is_ok());
        drop(third);
    }

    #[test]
    fn inflight_caps_are_per_leg() {
        let accounting = test_accounting().with_reservation_caps(ReservationCaps {
            per_peer: 1,
            global: 100,
        });
        let peer = test_peer();

        let _receive = accounting
            .prepare_receive(peer, au(10), true)
            .expect("receive slot");
        // The provide leg has its own count: a full receive cap does not gate it.
        let _provide = accounting
            .prepare_provide(peer, au(10))
            .expect("provide slot independent of receive");
        assert!(matches!(
            accounting.prepare_provide(peer, au(10)),
            Err(AccountingError::PeerInflightCap { .. })
        ));
    }

    #[test]
    fn global_inflight_cap_spans_peers_and_legs() {
        let accounting = test_accounting().with_reservation_caps(ReservationCaps {
            per_peer: 100,
            global: 2,
        });
        let peer1 = OverlayAddress::from([1u8; 32]);
        let peer2 = OverlayAddress::from([2u8; 32]);
        let peer3 = OverlayAddress::from([3u8; 32]);

        let held = accounting
            .prepare_receive(peer1, au(10), true)
            .expect("first global slot");
        let _provide = accounting
            .prepare_provide(peer2, au(10))
            .expect("second global slot");
        assert!(matches!(
            accounting.prepare_receive(peer3, au(10), true),
            Err(AccountingError::GlobalInflightCap { .. })
        ));

        drop(held);
        assert!(accounting.prepare_receive(peer3, au(10), true).is_ok());
    }

    #[test]
    fn receive_reserve_total_is_bounded_at_the_disconnect_line() {
        // Payment 1000, disconnect 1250. The peer owes us 10_000, so the
        // admission projection (reserved + price - balance) admits far past the
        // disconnect line; the absolute reserve bound is what refuses.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();
        accounting
            .for_peer(peer)
            .record(au(10_000), Direction::Upload);

        let _first = accounting
            .prepare_receive(peer, au(1000), true)
            .expect("within the reserve bound");
        let second = accounting
            .prepare_receive(peer, au(200), true)
            .expect("still within the reserve bound");
        assert!(matches!(
            accounting.prepare_receive(peer, au(100), true),
            Err(AccountingError::ReserveCap { .. })
        ));

        // Releasing a reservation restores reserve headroom.
        drop(second);
        assert!(accounting.prepare_receive(peer, au(100), true).is_ok());
    }

    #[test]
    fn provide_reserve_total_is_bounded_at_the_serve_line() {
        // We owe the peer 10_000, so the provide exposure (balance + shadow +
        // price) stays far below the serve line; the absolute reserve bound is
        // what refuses. Connect as a storer so the serve line is the full 1000.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();
        accounting.connect_peer(peer, SwarmNodeType::Storer);
        accounting
            .for_peer(peer)
            .record(au(10_000), Direction::Download);

        let first = accounting
            .prepare_provide(peer, au(600))
            .expect("within the reserve bound");
        assert!(matches!(
            accounting.prepare_provide(peer, au(600)),
            Err(AccountingError::ReserveCap { .. })
        ));

        drop(first);
        assert!(accounting.prepare_provide(peer, au(600)).is_ok());
    }

    #[test]
    fn held_receive_reservation_raises_projected_but_not_committed_debt() {
        // A held un-applied receive reservation raises the admission `project`
        // debt (it consumes headroom) but leaves the committed debt that drives
        // settlement unchanged, so a cheque never pays for a reservation that can
        // still drop.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        let reservation = accounting
            .prepare_receive(peer, au(800), true)
            .expect("within threshold");

        let balance = Ledger::balance(&accounting, &peer);
        let reserved = Ledger::reserved(&accounting, &peer);
        // Committed debt ignores the reservation (balance is still zero).
        assert_eq!(Debt::committed(balance), Debt::ZERO);
        // Projected debt for a further request includes the held reservation
        // (800 reserved + 100 price = 900), even though committed debt is zero.
        assert_eq!(Au::from(Debt::project(balance, reserved, au(100))), au(900));

        drop(reservation);
        assert_eq!(Ledger::reserved(&accounting, &peer), Au::ZERO);
    }

    #[test]
    fn prepare_receive_single_fetch_refuses_at_the_admit_boundary_for_a_fresh_peer() {
        // The single peer_state fetch bands off state_snapshot, so a fresh peer's
        // prepare_receive and the advisory admit refuse at exactly the same price.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        assert_ne!(
            accounting.admit(&peer, SMALL_DISCONNECT_THRESHOLD),
            Admission::Refuse
        );
        assert!(
            accounting
                .prepare_receive(peer, SMALL_DISCONNECT_THRESHOLD, true)
                .is_ok()
        );

        let over = SMALL_DISCONNECT_THRESHOLD + Au::new(1);
        assert_eq!(accounting.admit(&peer, over), Admission::Refuse);
        assert!(matches!(
            accounting.prepare_receive(peer, over, true),
            Err(AccountingError::DisconnectThreshold { .. })
        ));
    }

    #[test]
    fn state_snapshot_reproduces_ledger_snapshot_known_peer_fields() {
        // The one PeerState -> LedgerSnapshot mapping the receive gate and
        // Ledger::snapshot share must read the identical four fields.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        let handle = accounting.for_peer(peer);
        handle.record(au(500), Direction::Upload);
        let reservation = accounting
            .prepare_receive(peer, au(100), true)
            .expect("within threshold");

        let state = accounting.peer_state(peer);
        let direct = state_snapshot(&state, accounting.settle_trigger());
        let via_ledger = Ledger::snapshot(&accounting, &peer);

        assert_eq!(direct.balance, via_ledger.balance);
        assert_eq!(direct.reserved, via_ledger.reserved);
        assert_eq!(direct.disconnect_line, via_ledger.disconnect_line);
        assert_eq!(direct.settle_trigger, via_ledger.settle_trigger);

        drop(reservation);
    }

    // Default config: refresh 4_500_000, threshold 13_500_000, early 50%, so the
    // adopted-settle-line band is [9_000_000, 13_500_000].

    #[test]
    fn announced_threshold_within_band_is_adopted() {
        // An announcement inside the band sets the settle line verbatim, and the
        // derived per-peer settle trigger is its early-payment fraction floored at
        // one refresh unit: 9_000_000 * 50% = 4_500_000.
        let accounting = test_accounting();
        let peer = test_peer();

        accounting.adopt_settle_line(peer, au(9_000_000));

        assert_eq!(accounting.peer_state(peer).settle_line(), au(9_000_000));
        assert_eq!(Ledger::settle_trigger(&accounting, &peer), au(4_500_000));
    }

    #[test]
    fn announced_threshold_below_min_clamps_to_min() {
        // A hostile tiny announcement floors at 2 * refresh_rate = 9_000_000,
        // never driving our settle timing tighter than the minimum.
        let accounting = test_accounting();
        let peer = test_peer();

        accounting.adopt_settle_line(peer, au(1));

        assert_eq!(accounting.peer_state(peer).settle_line(), au(9_000_000));
    }

    #[test]
    fn announced_threshold_above_local_default_clamps() {
        // An announcement above the local payment threshold caps at it, so
        // adoption only ever tightens our settle line, never widens it.
        let accounting = test_accounting();
        let peer = test_peer();

        accounting.adopt_settle_line(peer, au(100_000_000));

        assert_eq!(accounting.peer_state(peer).settle_line(), au(13_500_000));
    }

    #[test]
    fn unannounced_peer_keeps_the_config_trigger() {
        // A peer that never announced reads the config-derived settle trigger, so
        // adoption changes nothing until an announcement lands. Both the map-miss
        // fallback and the lazily seeded settle line must yield it: the seed is
        // the full local payment threshold, not the tighter client serve line.
        let accounting = test_accounting();
        let peer = test_peer();

        let config = AccountingConfig::default();
        let expected = config.early_payment_trigger().max(config.refresh_rate());
        assert_eq!(Ledger::settle_trigger(&accounting, &peer), expected);

        accounting.peer_state(peer);
        assert_eq!(Ledger::settle_trigger(&accounting, &peer), expected);
    }

    #[test]
    fn adoption_never_touches_the_serve_line() {
        // The announcement writes only the settle line: the serve line the peer
        // owes us by stays at connect_peer's value, and a provide past it still
        // errs (direction-correctness). Connect as a client so the serve line
        // (200 under small_config) differs from the clamped adopted value (1000):
        // a cross-write onto the serve line moves an observable value here.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();
        accounting.connect_peer(peer, SwarmNodeType::Client);
        assert_eq!(accounting.peer_state(peer).serve_line(), au(200));

        accounting.adopt_settle_line(peer, au(1_000_000_000));

        assert_eq!(accounting.peer_state(peer).serve_line(), au(200));
        assert!(accounting.prepare_provide(peer, au(200)).is_ok());
        assert!(matches!(
            accounting.prepare_provide(peer, au(201)),
            Err(AccountingError::PaymentThreshold { .. })
        ));
    }

    #[test]
    fn connect_peer_reseed_does_not_clobber_adoption() {
        // small_config: refresh 0 so the adopted band is [0, 1000]. Adopting 500
        // and then connecting keeps the settle line (connect_peer writes only the
        // serve line); the reverse order keeps the serve line. The adoption and
        // connect-time serve-line writers touch disjoint fields, so they compose
        // without a race.
        let accounting = Accounting::new(small_config(), test_identity());

        // adopt then connect: settle line survives, serve line rises to storer.
        let a = OverlayAddress::from([1u8; 32]);
        accounting.adopt_settle_line(a, au(500));
        assert_eq!(accounting.peer_state(a).settle_line(), au(500));
        accounting.connect_peer(a, SwarmNodeType::Storer);
        assert_eq!(accounting.peer_state(a).settle_line(), au(500));
        assert_eq!(accounting.peer_state(a).serve_line(), au(1000));

        // connect then adopt: serve line survives the adoption.
        let b = OverlayAddress::from([2u8; 32]);
        accounting.connect_peer(b, SwarmNodeType::Storer);
        accounting.adopt_settle_line(b, au(500));
        assert_eq!(accounting.peer_state(b).settle_line(), au(500));
        assert!(accounting.prepare_provide(b, au(1000)).is_ok());
        assert!(matches!(
            accounting.prepare_provide(b, au(1001)),
            Err(AccountingError::PaymentThreshold { .. })
        ));
    }

    #[test]
    fn settlement_received_credits_and_accumulates_through_one_seam() {
        // The single settlement-received entry point: the ledger credit and the
        // cumulative repayment move together, and the returned total is the
        // running accumulator any settlement method feeds.
        let accounting = test_accounting();
        let peer = test_peer();
        let handle = accounting.for_peer(peer);
        handle.record(au(1000), Direction::Upload);

        let credit = handle.settlement_received(au(300));
        assert_eq!(credit.cumulative_repayment, au(300));
        assert_eq!(credit.raised_serve_line, None);
        assert_eq!(handle.balance(), au(700));

        assert_eq!(
            handle.settlement_received(au(200)).cumulative_repayment,
            au(500)
        );
        assert_eq!(handle.balance(), au(500));
        assert_eq!(accounting.peer_state(peer).cumulative_repayment(), au(500));
    }

    #[test]
    fn repayment_past_the_checkpoint_raises_the_enforced_serve_line() {
        // small_config with a positive refresh rate: storer line 1000, rate 10,
        // first checkpoint 1000. Repayment strictly past it raises the serve
        // line one rate step, the raise is the line prepare_provide enforces,
        // and the returned raised line reads back that same state.
        let config =
            AccountingConfig::new(1000, 25, 10, 0, 5, crate::FixedPricingConfig::default());
        let accounting = Accounting::new(config, test_identity());
        let peer = test_peer();
        accounting.connect_peer(peer, SwarmNodeType::Storer);
        let handle = accounting.for_peer(peer);

        // Build up debt so repayment has something to credit against.
        handle.record(au(1_001), Direction::Upload);

        // At the checkpoint exactly: no raise.
        assert_eq!(
            handle.settlement_received(au(1_000)).raised_serve_line,
            None
        );
        assert_eq!(accounting.peer_state(peer).serve_line(), au(1_000));

        // One past it: raised by one rate step and enforced by the provide gate
        // (the balance is fully repaid, so the exposure is the price alone).
        let credit = handle.settlement_received(au(1));
        assert_eq!(credit.raised_serve_line, Some(au(1_010)));
        assert_eq!(handle.balance(), Au::ZERO);
        assert!(accounting.prepare_provide(peer, au(1_010)).is_ok());
        assert!(matches!(
            accounting.prepare_provide(peer, au(1_011)),
            Err(AccountingError::PaymentThreshold { .. })
        ));
    }

    #[test]
    fn growth_steps_are_keyed_on_the_remote_type_rate() {
        // Default config on a client remote: allowance rate 450_000, so the
        // first checkpoint is 45_000_000 and a raise is one scaled step, while
        // a storer remote grows in full-rate steps from the same node.
        let accounting = test_accounting();
        let client = OverlayAddress::from([1u8; 32]);
        let storer = OverlayAddress::from([2u8; 32]);
        accounting.connect_peer(client, SwarmNodeType::Client);
        accounting.connect_peer(storer, SwarmNodeType::Storer);

        for peer in [client, storer] {
            accounting
                .for_peer(peer)
                .record(au(1_000_000_000), Direction::Upload);
        }

        let client_handle = accounting.for_peer(client);
        assert_eq!(
            client_handle
                .settlement_received(au(45_000_001))
                .raised_serve_line,
            Some(au(1_350_000 + 450_000))
        );

        let storer_handle = accounting.for_peer(storer);
        assert_eq!(
            storer_handle
                .settlement_received(au(450_000_001))
                .raised_serve_line,
            Some(au(13_500_000 + 4_500_000))
        );
    }

    #[test]
    fn reconnect_resets_the_serve_line_and_growth() {
        // A raise earned on one connection does not survive a reconnect: the
        // connect hook returns the serve line to the node-type default and
        // restarts the accumulator and checkpoint, so growth is earned afresh.
        let config =
            AccountingConfig::new(1000, 25, 10, 0, 5, crate::FixedPricingConfig::default());
        let accounting = Accounting::new(config, test_identity());
        let peer = test_peer();
        accounting.connect_peer(peer, SwarmNodeType::Storer);
        let handle = accounting.for_peer(peer);

        handle.record(au(2_000), Direction::Upload);
        assert_eq!(
            handle.settlement_received(au(1_001)).raised_serve_line,
            Some(au(1_010))
        );

        // Reconnect: back to the default line, zero accumulator, first checkpoint.
        assert_eq!(
            accounting.connect_peer(peer, SwarmNodeType::Storer),
            au(1_000)
        );
        let state = accounting.peer_state(peer);
        assert_eq!(state.cumulative_repayment(), Au::ZERO);
        assert_eq!(state.growth_checkpoint(), au(1_000));

        // The same crossing must be re-earned on the new connection.
        handle.record(au(2_000), Direction::Upload);
        assert_eq!(
            handle.settlement_received(au(1_001)).raised_serve_line,
            Some(au(1_010))
        );
    }

    #[test]
    fn connect_peer_keys_the_allowance_rate_on_the_remote_type() {
        // Default config: base refresh 4_500_000, factor 10. The allowance rate
        // heals with the serve line at connect, keyed on the remote's type from
        // the unscaled base; a peer never connected keeps the conservative
        // client-rate seed.
        let accounting = test_accounting();
        let storer = OverlayAddress::from([1u8; 32]);
        let client = OverlayAddress::from([2u8; 32]);
        let unknown = OverlayAddress::from([3u8; 32]);

        accounting.connect_peer(storer, SwarmNodeType::Storer);
        accounting.connect_peer(client, SwarmNodeType::Client);

        assert_eq!(accounting.for_peer(storer).allowance_rate(), au(4_500_000));
        assert_eq!(accounting.for_peer(client).allowance_rate(), au(450_000));
        assert_eq!(accounting.for_peer(unknown).allowance_rate(), au(450_000));
    }

    #[test]
    fn allowance_rate_on_a_client_node_derives_from_the_unscaled_base() {
        // for_client scales only the debtor pacing rate; the allowance extended
        // to a storer remote stays the full base rate.
        let accounting = Accounting::new(client_config(), test_identity());
        let storer = OverlayAddress::from([1u8; 32]);
        accounting.connect_peer(storer, SwarmNodeType::Storer);

        assert_eq!(accounting.for_peer(storer).allowance_rate(), au(4_500_000));
    }

    #[test]
    fn adopted_threshold_moves_the_admission_band() {
        // Adopting 9_000_000 lowers the settle trigger from the config 6_750_000
        // to 4_500_000, so a projected debt of 5_000_000 that Admits on an
        // un-announced peer becomes SettleAndAdmit on the adopted one. The serve
        // line (and thus prepare_provide) is unchanged.
        let accounting = test_accounting();
        let adopted = OverlayAddress::from([1u8; 32]);
        let control = OverlayAddress::from([2u8; 32]);

        accounting.adopt_settle_line(adopted, au(9_000_000));

        assert_eq!(accounting.admit(&control, au(5_000_000)), Admission::Admit);
        assert_eq!(
            accounting.admit(&adopted, au(5_000_000)),
            Admission::SettleAndAdmit
        );

        // The client serve line (13_500_000 / 10) is untouched by adoption.
        assert!(accounting.prepare_provide(adopted, au(1_350_000)).is_ok());
        assert!(matches!(
            accounting.prepare_provide(adopted, au(1_350_001)),
            Err(AccountingError::PaymentThreshold { .. })
        ));
    }

    // --- Persistence ------------------------------------------------------

    use crate::persistence::DbBalanceStore;

    fn shared_db() -> Arc<vertex_storage_redb::RedbDatabase> {
        vertex_storage_redb::RedbDatabase::in_memory()
            .unwrap()
            .into_arc()
    }

    fn persistent_accounting(
        db: &Arc<vertex_storage_redb::RedbDatabase>,
    ) -> Accounting<AccountingConfig, Identity> {
        let store = DbBalanceStore::new(db.clone());
        store.init().unwrap();
        let accounting = Accounting::new(AccountingConfig::default(), test_identity())
            .with_balance_store(Some(Arc::new(store)));
        accounting.restore().unwrap();
        accounting
    }

    #[test]
    fn restart_round_trip_restores_debtor_and_creditor_balances() {
        let db = shared_db();
        let debtor = OverlayAddress::from([1u8; 32]);
        let creditor = OverlayAddress::from([2u8; 32]);

        {
            let accounting = persistent_accounting(&db);
            // We owe the debtor peer (negative), the creditor owes us (positive).
            accounting
                .for_peer(debtor)
                .record(au(3_000), Direction::Download);
            accounting
                .for_peer(creditor)
                .record(au(5_000), Direction::Upload);
            accounting.flush().unwrap();
        }

        // Reconstruct over the same database: balances survive the restart.
        let reloaded = persistent_accounting(&db);
        assert_eq!(reloaded.balance(&debtor), au(-3_000));
        assert_eq!(reloaded.balance(&creditor), au(5_000));
    }

    #[test]
    fn flush_persists_only_dirty_peers_and_a_later_apply_rearms() {
        let db = shared_db();
        let peer_a = OverlayAddress::from([1u8; 32]);
        let peer_b = OverlayAddress::from([2u8; 32]);

        let accounting = persistent_accounting(&db);
        accounting
            .for_peer(peer_a)
            .record(au(1_000), Direction::Upload);
        accounting.flush().unwrap();

        // A no-op flush (nothing dirty) writes nothing and must not clobber A.
        accounting.flush().unwrap();

        // A later apply on B re-arms the dirty set; the next flush picks it up.
        accounting
            .for_peer(peer_b)
            .record(au(2_000), Direction::Upload);
        accounting.flush().unwrap();

        let reloaded = persistent_accounting(&db);
        assert_eq!(reloaded.balance(&peer_a), au(1_000));
        assert_eq!(reloaded.balance(&peer_b), au(2_000));
    }

    #[test]
    fn restore_without_a_store_is_a_noop() {
        let accounting = test_accounting();
        assert_eq!(accounting.restore().unwrap(), 0);
        // Flush without a store must also be a harmless no-op.
        accounting.flush().unwrap();
    }

    #[test]
    fn restored_balance_is_not_marked_dirty() {
        let db = shared_db();
        let peer = OverlayAddress::from([7u8; 32]);

        {
            let accounting = persistent_accounting(&db);
            accounting
                .for_peer(peer)
                .record(au(1_500), Direction::Upload);
            accounting.flush().unwrap();
        }

        // On reload the restored peer is clean, so the first flush writes nothing
        // new; the store still holds the original balance.
        let reloaded = persistent_accounting(&db);
        assert!(
            !reloaded.peer_state(peer).take_dirty(),
            "a restored balance must not be dirty"
        );
        assert_eq!(reloaded.balance(&peer), au(1_500));
    }
}
