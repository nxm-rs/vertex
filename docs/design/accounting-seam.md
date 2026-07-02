# Accounting seam

The trait and type surface of bandwidth accounting, designed from the responsibilities rather than grown from them. Companion to `accounting-settlement.md` (the provider substrate) and `client-credit-admission.md` (the client-side band policy that consumes this seam).

## Responsibilities

Accounting does five things and nothing else:

1. Ledger. A signed per-peer balance in AU (`+` the peer owes us, `-` we owe), committed by `record`, read as a balance or as a non-negative `Debt`.
2. Reservation. In-flight holds so our debt view matches the creditor's shadow reserve: a receive leg (we owe) and a provide leg (they owe), each reserve then apply-on-success then drop-to-release.
3. Admission. The band decision over committed plus reserved against the payment and disconnect thresholds.
4. Settlement. A composable provider fan-out (pseudosettle, then swap) that reduces committed debt, plus the single-in-flight settle per peer.
5. Scoring. Reporting genuine peer misbehaviour, never our own debt. Scoring stays the peer manager's authority and is not part of this seam.

## Types

- `Au` (existing): signed `i64`, `+` = the peer owes us. Carries balance, price, threshold, settled amount. Free arithmetic.
- `Debt` (new): an opaque non-negative newtype (`Debt(u64)`, private field), `Ord`, `From<Debt> for Au` (total). It deliberately has NO `Add`/`Sub`/`Neg` against `Au` and NO `From<Au>`: a signed balance can never be fed in, so the sign error that compared a negative balance to a positive threshold is unrepresentable. Two named constructors, because admission and settlement measure different things:
  - `Debt::committed(balance) = max(0, -balance)`: what we have actually committed to owing. SETTLEMENT uses this. A swap cheque is real money, so we must never pay for in-flight reservations that can still drop.
  - `Debt::project(balance, reserved, price) = max(0, reserved + price - balance)`: what we would owe including the pending reservation and this request. ADMISSION uses this.
  Methods: `saturating_sub(Au) -> Debt`, `exceeds(Au) -> bool`. The constructor sign logic lives in one place and is pinned by a property test.
- `Admission` (new): `#[derive(Clone, Copy)]` enum `Admit | SettleAndAdmit | Refuse`, with `strum::IntoStaticStr` for metric labels and `admits()`/`settles()` helpers. No payload.
- `Reservation<Leg>` (new): RAII hold, with zero-cost typestate markers `Receive` and `Provide`. `apply(self)` commits the leg; `Drop` releases the leg's reserved counter (`reserved_balance` for receive, `shadow_reserved_balance` for provide). A leg mismatch is a compile error, not a runtime branch. The single deferred-commit boundary (the forwarder hands the provide leg to the wire-write site and commits it after the bytes land) survives as one object-safe trait `CommitOnWrite` that only `Reservation<Provide>` implements; it lives in the api crate so `ForwardedChunk` can hold `Box<dyn CommitOnWrite>`.

## The trait stack

Small single-concern leaf traits composed by supertrait, with one aggregate handle. All in `vertex-swarm-api`.

```
#[auto_impl(&, Arc, Box)]
trait Ledger: Send + Sync {
    fn balance(&self, peer: &OverlayAddress) -> Au;
    fn reserved(&self, peer: &OverlayAddress) -> Au;
    fn headroom(&self, peer: &OverlayAddress, to: Threshold) -> Au;  // unifies the two allowance_* reads, floored
}
enum Threshold { Payment, Disconnect }

trait AdmissionControl: Ledger {
    fn admit(&self, peer: &OverlayAddress, price: Au) -> Admission { /* default: project debt, band it */ }
}
impl<T: Ledger> AdmissionControl for T {}   // one blanket impl; no second impl (E0119)
```

The blanket impl is the only `AdmissionControl` impl, so a `Ledger` cannot override `admit`; the band stays single-sourced. The existing node-side settlement trigger (`AccountingSettlement` in `swarm/node/selection.rs`) is the spawn path, not a ledger method, so no `Reserve`/`Settle`/`ClientAccounting` aggregate trait is introduced.

`admit` is a default method on `AdmissionControl` over `Ledger`, so the band lives in exactly one place and every ledger gets it for free. It replaces `can_afford`, `allowance_remaining`, `allowance_to_payment_threshold`, and `should_settle`. This sweep ships the `Ledger` and `AdmissionControl` traits and the `Reservation` typestate; it does NOT introduce the `Reserve`, `Settle`, or aggregate `ClientAccounting` traits or an `AccountingHandle`, because settlement stays a node-triggered fan-out (see Settlement below). The selector and the origin credit gate on `ClientHandle` hold `Arc<dyn AdmissionControl>` (which carries `admit` plus the `Ledger` `balance`/`reserved` reads), alongside the existing node-side settlement trigger, the pricer (`SwarmPricing`), and the scores (`PeerScores`). `auto_impl` makes the boxed `Ledger` re-satisfy the trait so generic test mocks and the production handle share one bound. Pricing and scoring (`PeerScores`/`PeerReporter`) stay separate concerns, not folded in.

## Admission and the hard gate share one boundary

`admit` is the advisory query the selector consumes; `prepare_receive` is the hard gate that returns an error and blocks a debit. The original bug was two copies of a threshold formula that drifted. So both MUST compute from the same `Debt::project` constructor and compare against the same `Threshold` headroom: there is one boundary expression, and `admit` and `prepare_receive` cannot diverge. If they are ever kept separate, a boundary-equivalence test is mandatory, not optional.

## Settlement

Settlement is unchanged by this sweep and stays exactly as it ships. The providers are held as `Arc<[Box<dyn SwarmSettlementProvider>]>` and the fan-out (`settle_all`) is an `async_trait` method in `accounting-core`. The reshape does NOT introduce a concrete `Settlement` enum, native AFIT, or a `settle_until` combinator: a closed enum that names `PseudosettleProvider`/`SwapProvider` in core would create a Cargo cycle (core would depend on the provider crates that already depend on core), so it is not done. The `Box<dyn ...>` trait-object surface has no such cycle.

The provider signature is unchanged: `settle(&self, peer: OverlayAddress, state: &dyn SwarmPeerState) -> SwarmResult<Au>` (async via `async_trait`). It still reads `balance()` through the now-`balance()`-only `SwarmPeerState` for its own recheck; do not pass `Debt` to providers.

What the sweep does change is the `settle_all` break reason: it reasons in `Debt::committed`, sign-safe by construction, instead of comparing a signed balance to a positive threshold:

```
total = total.saturating_add(provider.settle(peer, state).await?);
if !Debt::committed(state.balance()).exceeds(payment_threshold) { break; }   // both non-negative
```

Two correctness invariants that the providers and the credit timing already satisfy and must keep:

- Each provider re-reads `state.balance()` and bails if non-negative. This is the guard that absorbs a torn read of the two separate atomics (`balance`, `reserved`) and ensures a swap cheque is only ever issued against actually-committed debt.
- The pseudosettle and swap services credit the ledger synchronously before resolving the settle future (they `record` the accepted amount, then fulfil the oneshot the provider awaits). So the fan-out recomputes committed `Debt` on fresh state between providers, and the break fires correctly.

## Single-in-flight settle, owned by the node trigger

The fire-and-forget settle trigger stays node-side exactly as it ships: `AccountingSettlement` (in `swarm/node/selection.rs`) checks `TaskExecutor::try_current()`, dedups per peer through a shared in-flight set, and spawns the settle on the current executor; an `InFlightGuard` clears the peer on drop so a panicking or cancelled future cannot pin it. This sweep does NOT move that state into the ledger as a `begin_settle`/`SettleGuard` `AtomicBool`. A bounding timeout on the settle future (a withholding peer pinning the dedup guard) is a pre-existing item tracked separately, not introduced here.

wasm imposes no constraint on settlement: `vertex_tasks` and `web-time` are wasm-safe and CI builds the accounting cone for `wasm32`, so the spawning provider/trigger paths are not a barrier to a future enum, were one ever wanted. The Cargo cycle, not wasm, is the reason the trait-object surface stays.

## Peer state

`PeerState` carries `balance`, `reserved_balance`, `shadow_reserved_balance`, `ghost_balance`, and the two thresholds. The dead fields (`last_refresh`/`set_last_refresh`, `surplus_balance`/`add_surplus`, `set_balance`, and the `snapshot`/`from_snapshot`/`PeerStateSnapshot` path) were removed in the prior deletions sweep. The `SwarmPeerState` trait is `balance()`-only (the only method read through it), which the settlement provider uses for its recheck.

Ghost debt is the trace of refused deliveries. A provide reservation released because the peer refused to take the answer off the wire (`CommitOnWrite::forfeit_boxed`, only ever called by the handler arms where the answer was in hand and the write back failed) accrues its price into `ghost_balance` instead of vanishing. The ghost is never committed and never settled; it participates only in the `prepare_provide` projection (`balance + shadow_reserved + ghost + price` against the payment threshold), so a peer that repeatedly requests answers and refuses delivery starves out of service instead of draining relay legs for free. Failures on our side of a relay drop the reservation as before, releasing without a trace.

## Removed dead code

Confirmed by census, none with a live non-test caller: the `pre_allow` chain end to end (`SwarmSettlementProvider::pre_allow`, `SwarmPeerBandwidth::allow`, `pre_allow_all`); `SwarmClientAccounting::receive_price`/`provide_price` and its default forwarders; `Accounting::config()`/`providers()` accessors; `get_or_create_client_peer`/`PeerState::new_client_only` (client scaling lives in `BandwidthConfig::for_client`); the dead `PeerState` fields above. The old `AccountingAction` trait splits into two: `Commit` (the by-value receive apply, committed the moment the chunk is in hand) and the object-safe `CommitOnWrite` (the deferred provide commit the forwarder boxes into `ForwardedChunk`/`ForwardedReceipt`). `prepare_provide`/`shadow_reserved` stay live (the client relays through `NetworkForwarder`).

## What lands when

Three changes, in order, because each depends on the prior:

1. Cleanup sweep (this seam, shipped). Introduce the `Ledger`/`AdmissionControl` traits, the `Debt` and `Admission` value types, and the `Reservation` typestate; wire `admit` to today's committed-balance band; reason the `settle_all` break in `Debt::committed`. Settlement dispatch and the node trigger stay as shipped. The `PeerInflightLimiter` depth cap stays a separate concurrency axis.
2. Reserve as gate (shipped). Reserve-at-dispatch on origin retrieval AND origin pushsync (previously only the forwarder relay legs reserved); `admit` drives the band: `SettleAndAdmit` through the tolerance band (pushsync sends to its closest peer and settles in parallel, preserving closeness), `Refuse`-skip only at the disconnect line; sized on the client thresholds. The self-throttle was removed once this landed: the band is the synchronous brake, and accounting-administered pacing against the real ledger supersedes the throttle's modelled-rate GCRA. The `Ledger::headroom` read and the `Threshold` enum the throttle alone consumed went with it. `PeerInflightLimiter` stays.
3. Pre-pay and pacing (subsumed). The band's `SettleAndAdmit` settles before admitting through the whole tolerance band, so the separate pre-pay step and the throttle allowance-percent knob are subsumed; the allowance-percent knob was removed with the throttle.

## Non-goals

No wire-byte change. No change to creditor-serving behaviour. Settlement dispatch keeps the `Box<dyn SwarmSettlementProvider>` fan-out; a swap-off build simply registers no swap provider.
