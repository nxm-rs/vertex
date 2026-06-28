# Client credit admission and settlement

## Scope

How a client bounds and settles its per-peer debt during retrieval and pushsync so a storer never disconnects it for unpaid debt. This layers on the mechanism-agnostic accounting and settlement substrate (see `accounting-settlement.md`); the concern here is the client-side coordination: reservation, settle triggering, and candidate filtering.

A client meters its debt against the line a storer enforces on it: the storer line divided by the client factor (see `BandwidthConfig::for_client`). The only per-peer brake a storer applies is accounting debt; cross its disconnect line and it resets the connection and blocklists us. So a client must hold per-peer unsettled debt under that line at all times, including across separate requests, not just within one.

## One surface, shared by retrieval and pushsync

Candidate selection for both retrieval and pushsync already runs through `PeerSelector`, which ranks proximity-ordered peers by score and affordability. The credit-admission surface extends what is already there rather than adding a parallel mechanism:

- `PeerAffordability` answers "can this peer take another request now" from committed balance plus in-flight reservation, measured against the client threshold.
- `PeerSelector` hard-skips a peer that cannot, so the request routes to the next-closest peer while a background settle drains the skipped one.
- A peer with a settle already in flight is in a settle-pending state and is skipped on the same path.

Because both protocols consume `PeerSelector`, the filter, the reservation accounting, and the settle-pending state are defined once and apply to both.

## The reserve, settle, skip chain

A request is admitted by reserving its price against two thresholds a tolerance band apart: the payment threshold (the point at which we should settle) and the disconnect line (the point at which a storer drops us). The gap between them is the storer's own tolerance, and it is the margin that lets us settle and request back to back without waiting.

1. Reserve at dispatch. Reserve the chunk price for the in-flight leg, with the same reserve then apply-on-delivery then drop-on-failure shape the forwarder uses for its two legs, so committed-plus-reserved debt reflects outstanding requests the way a storer's shadow reserve does.
2. Below the payment threshold. Admit; no settle is needed.
3. In the tolerance band (payment threshold up to the disconnect line). Trigger a single-in-flight settle and admit the request immediately rather than waiting for the settle to complete. This is safe whatever order the storer processes them in: the settle and the request travel on separate substreams with no cross-stream ordering guarantee, but the debt is still under the disconnect line even if the request is metered before the settle is applied. The settle, often applied first, hands back allowance; the band is the margin that covers us when it is not. Correctness comes from the margin, not from the processing order.
4. At the disconnect line. The reserve fails. The peer is hard-skipped so the chunk routes to another peer while a background settle drains it, because here admitting could cross the line before the settle lands. While its settle is in flight the peer is settle-pending and selection skips it; once the settle is acked and the debt drops back into the band, it is admissible again.

So the settle-and-request-immediately path runs through the whole tolerance band, keeping the closest peers in play at no added latency, and a peer is shed only when its debt is genuinely at the line.

## Single in-flight settle: dedup, rate-limit, and pending-state in one

At most one settlement is in flight to any one peer. This single rule provides three properties that would otherwise need three mechanisms:

- Dedup. Hundreds of admission failures per second against the same peer collapse to one settle, so the single-thread executor is never flooded with redundant settle futures.
- Rate-limit bound to acceptance. The next settle to a peer cannot start until the previous one is acked, so the client physically cannot send settlements faster than the storer refreshes its allowance.
- Settle-pending state. "A settle is in flight" is exactly the predicate selection uses to skip the peer.

A settle offers the whole outstanding debt over the wire; once acked, the debt drops below the early-payment trigger and the peer is admissible again.

## Mechanism-agnostic settlement

Settlement always goes through the accounting settle entry point, which fans out to every registered provider in registration order: pseudosettle forgives the time-based allowance first, then swap (when compiled and enabled) settles the originated remainder, with an early break once the balance clears the payment threshold. The client-credit surface never calls a settlement provider directly, so a swap-enabled build settles through swap with no change to this layer, and a swap-off client speaks only pseudosettle and is a normal peer.

## A debtor does not blame its creditor

Crossing our own debt line is our own failure to settle in time, not peer misbehaviour. The receive path refuses at the line so the request routes elsewhere, but it does not score the peer down. Scoring stays for genuine misbehaviour on other paths: over-acceptance on the settlement wire and malformed chunks.

## Delivery order

The surface lands in layers, each independently useful and shippable:

1. Reliable settlement. A deduped single-in-flight settle trigger that fires on a debt-threshold approach, not only when every candidate is already unaffordable, routed through the provider fan-out. Local time-refresh of our own debt is removed, so our debt view matches the storer's rather than masking it.
2. No self-scoring. The receive path refuses at the line without scoring the peer.
3. Reserve as gate. Reservation at dispatch for retrieval and origin pushsync; reservation failure drives settle-and-skip on the shared selector.
4. Pre-pay and pacing. Settle a peer past the trigger before admitting the next request, and pace the full threshold headroom once the gate guarantees the margin.

## Terminology and non-goals

Node types are client and storer; the client factor scales a client's thresholds below a storer's. This is client-side debt management only: it does not change wire bytes, and it does not alter how the node behaves as a creditor serving others.
