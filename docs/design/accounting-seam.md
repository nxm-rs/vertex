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
    fn debt(&self, peer: &OverlayAddress) -> Debt;            // committed
    fn headroom(&self, peer: &OverlayAddress, to: Threshold) -> Au;  // unifies the two allowance_* reads
}
enum Threshold { Payment, Disconnect }

#[auto_impl(&, Arc, Box)]
trait AdmissionControl: Ledger {
    fn admit(&self, peer: &OverlayAddress, price: Au) -> Admission { /* default: project debt, band it */ }
}
impl<T: Ledger> AdmissionControl for T {}

trait Reserve: Send + Sync {
    fn reserve_receive(&self, peer: OverlayAddress, price: Au) -> SwarmResult<Reservation<Receive>>;
    fn reserve_provide(&self, peer: OverlayAddress, price: Au) -> SwarmResult<Reservation<Provide>>;
}

#[auto_impl(&, Arc, Box)]
trait Settle: Send + Sync { fn trigger_settle(&self, peer: OverlayAddress); }

#[auto_impl(&, Arc, Box)]
trait ClientAccounting: AdmissionControl + Reserve + Settle {}
impl<T: AdmissionControl + Reserve + Settle> ClientAccounting for T {}
```

`admit` is a default method on `AdmissionControl` over `Ledger`, so the band lives in exactly one place and every ledger gets it for free. It replaces `can_afford`, `allowance_remaining`, `allowance_to_payment_threshold`, and `should_settle`. The selector (and, while it exists, the self-throttle) hold ONE `Arc<dyn ClientAccounting>` erased once at the node-type boundary, instead of today's four separate trait objects. `auto_impl` makes the boxed handle re-satisfy the trait so generic test mocks and the production handle share one bound. Pricing (`SwarmPricing`) and scoring (`PeerScores`/`PeerReporter`) stay separate concerns, not folded in.

## Admission and the hard gate share one boundary

`admit` is the advisory query the selector consumes; `prepare_receive` is the hard gate that returns an error and blocks a debit. The original bug was two copies of a threshold formula that drifted. So both MUST compute from the same `Debt::project` constructor and compare against the same `Threshold` headroom: there is one boundary expression, and `admit` and `prepare_receive` cannot diverge. If they are ever kept separate, a boundary-equivalence test is mandatory, not optional.

## Settlement

The provider signature drops `pre_allow` and the mutable `&dyn SwarmPeerState` argument, and takes committed debt:

```
fn settle(&self, peer: OverlayAddress, debt: Debt) -> impl Future<Output = SwarmResult<Au>> + Send
```

Dispatch is a cfg-gated enum over the closed provider set, not a `Vec<Box<dyn ...>>`, so the method is native AFIT (no `async_trait`) and a swap-off build is a one-variant enum:

```
enum Settlement { Pseudosettle(PseudosettleProvider), #[cfg(feature = "swap")] Swap(SwapProvider) }
```

The fan-out is a method that reasons in `Debt`, sign-safe by construction:

```
async fn settle_until(&self, peer, mut debt: Debt /* committed */, floor: Au) -> SwarmResult<Au> {
    let mut total = Au::ZERO;
    for step in &self.steps {
        total = total.saturating_add(step.settle(peer, debt).await?);
        debt = debt.saturating_sub(total);
        if !debt.exceeds(floor) { break; }      // both non-negative
    }
    Ok(total)
}
```

Two correctness invariants that the providers and the credit timing already satisfy and must keep:

- Each provider re-reads `state.balance()` and bails if non-negative. This is the guard that absorbs a torn read of the two separate atomics (`balance`, `reserved`) and ensures a swap cheque is only ever issued against actually-committed debt. The `Debt` passed in is advisory; the balance recheck is authoritative for payment. Do not remove it when dropping the state argument from the trait: hand the provider the committed `Debt` and let it confirm against live balance.
- The pseudosettle and swap services credit the ledger synchronously before resolving the settle future (they `record` the accepted amount, then fulfil the oneshot the provider awaits). So `settle_until` recomputes `Debt` on fresh state between providers, and the break fires correctly.

## Single-in-flight settle, owned by the ledger

The settle-pending state is a per-peer `AtomicBool` in `PeerState` (the ledger), not a set beside it. The ledger exposes an RAII acquire:

```
fn begin_settle(&self, peer: &OverlayAddress) -> Option<SettleGuard>   // CAS the flag; None if already set
// SettleGuard::Drop clears the flag.
```

The node layer keeps the ordering that already exists today: check `TaskExecutor::try_current()` FIRST; only on success call `begin_settle`; move the returned guard into the spawned settle future. Any early return (no executor, spawn failure) drops the owned guard and clears the flag, so the flag can never leak set with no future to clear it. The settle future is bounded with a `vertex_tasks::time` timeout, so a peer that takes the request but never acks cannot pin the flag forever and starve its own settlement. The spawn and the executor stay in the node layer; the ledger holds only the `AtomicBool` (wasm-clean, no tokio). Note there remain two in-flight surfaces (this flag and the pseudosettle/swap service `pending` maps); they can momentarily disagree on a cancelled future plus a late ack, which is benign and documented.

## Peer state

`PeerState` shrinks to `balance`, `reserved_balance`, `shadow_reserved_balance`, the two thresholds, and the `settle_pending` `AtomicBool`. Removed: `last_refresh`/`set_last_refresh` (time-refresh is gone), `surplus_balance`/`add_surplus`, `set_balance`, and the `snapshot`/`from_snapshot`/`PeerStateSnapshot` path (no caller; balances are not persisted). The `SwarmPeerState` trait collapses (only `balance()` was read through it).

## Removed dead code

Confirmed by census, none with a live non-test caller: the `pre_allow` chain end to end (`SwarmSettlementProvider::pre_allow`, `SwarmPeerBandwidth::allow`, `pre_allow_all`); `SwarmClientAccounting::receive_price`/`provide_price` and its default forwarders; `Accounting::config()`/`providers()` accessors; `get_or_create_client_peer`/`PeerState::new_client_only` (client scaling lives in `BandwidthConfig::for_client`); the dead `PeerState` fields above. The `AccountingAction` object-safe trait stays as `CommitOnWrite` (renamed) for the one deferred provide commit. `prepare_provide`/`shadow_reserved` stay live (the client relays through `NetworkForwarder`).

## What lands when

Three changes, in order, because each depends on the prior:

1. Cleanup sweep (this seam). Introduce the trait stack, `Debt`/`Admission`/`Reservation` typestate/`Settlement` enum, wire `admit` to today's committed-balance band, remove the dead code. KEEP the self-throttle (it reads `headroom(Payment)` through the handle) and the `PeerInflightLimiter` depth cap. No behaviour change to pacing.
2. Reserve as gate. Reserve-at-dispatch on origin retrieval AND origin pushsync (today only the forwarder relay legs reserve); `admit` drives the band: `SettleAndAdmit` through the tolerance band (pushsync sends to its closest peer and settles in parallel, preserving closeness), `Refuse`-skip only at the disconnect line; sized on the client thresholds. THEN remove the self-throttle in the same change, since its synchronous brake is now provided by the gate, and accounting-administered pacing against the real ledger supersedes the throttle's modelled-rate GCRA. Keep `PeerInflightLimiter` (a separate concurrency axis). Removing the throttle before this point would open an unpaced burst window.
3. Pre-pay and pacing. The band's `SettleAndAdmit` already settles before admitting through the whole tolerance band, so the separate pre-pay step and the throttle allowance-percent tuning are largely subsumed; reassess what, if anything, remains once the gate lands.

## Non-goals

No wire-byte change. No change to creditor-serving behaviour. Mechanism-agnostic: a swap-off build is a one-variant `Settlement` enum.
