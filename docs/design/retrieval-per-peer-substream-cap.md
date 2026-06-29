# Retrieval fan-out: per-peer substream cap and unordered delivery

Status: accepted (design)
Area: `vertex-swarm-node` retrieval, `vertex-swarm-stream`

## Context

Under a sustained download the retrieval fan-out overruns the per-connection substream budget that the reference peer enforces. Each chunk races only its closest few peers, and address proximity clusters those races onto the same handful of neighbourhood peers, so one hot peer accumulates many simultaneous outbound retrieval substreams and is reset. The reset cascade drains the close bins below saturation and collapses routing depth mid-download. This is the non-economic half of the neighbourhood-collapse symptom; the economic half (per-peer debt crossing the remote disconnect line) is tracked separately and must not be conflated with it.

## The unordered-delivery invariant (load-bearing)

Within vertex, chunk delivery is unordered. The get pipeline yields chunks in completion (arrival) order, never request order, and every item is self-describing: it carries its own `ChunkAddress`. Any caller that needs file or byte order MUST reorder downstream. The reference consumer is the nectar `WindowedReader`, which lands resolved leaves in a `BTreeMap` keyed by absolute offset and emits the head only when its offset reaches the next emit position, bounding memory by a fixed window regardless of arrival order. The gRPC chunk adapter relies on the same property via `buffer_unordered`.

This invariant is what makes the fan-out free to redirect a chunk to whichever close peer has a free slot, in any completion order. Skip-busy, the staggered race, and narrower concurrency are all pure performance levers only because of it. No retrieval-side change may assume or reintroduce ordered delivery; doing so turns the concurrency knobs into correctness-affecting parameters and corrupts output under skip-busy.

## Concurrency taxonomy (retire "width")

The term "prefetch width" is retired. Three distinct, independently named concerns:

- Pipeline depth: `StreamConfig::max_concurrency` (32 default, `NATIVE_DOWNLOAD_CONCURRENCY` for bulk). The total in-flight retrieval pool, a bandwidth-delay-product and memory ceiling. It bounds total concurrency, not per-peer concurrency, so it is NOT the overrun guard.
- Per-peer in-flight cap: the new overrun guard. Bounds concurrent outbound retrieval substreams to any single peer, keeping us under the remote's per-connection multiplexer budget.
- Reorder window: the consumer-side memory bound (nectar `WindowedReader`), independent of arrival order.

The overrun bug was having only the first and not the second.

## The fix: per-peer cap + skip-busy spread

Bound concurrent outbound retrieval substreams per peer with a non-blocking permit. When a peer is at its cap the scheduler SKIPS it and dispatches the chunk to the next-closest live peer that has a free slot, rather than blocking on the head peer (which would be head-of-line blocking and defeat the staggered race). The permit rides the request future and releases on drop, including a cancelled race leg. Candidate selection widens from the closest few to the closest connected peers in proximity order, filtered to those with a free slot, then fed to the existing staggered race. If every close candidate is at its cap, the dispatch falls through to the staggered race against whatever peers exist rather than erroring (degraded service beats failure).

The cap is a non-economic concern, composed after economic selection. The explicit dispatch ordering is: selector decides who is eligible (proximity, not warned, affordable), skip-busy decides who has a slot, the origin credit gate bands the chosen request. The substream cap must never be merged with the affordability or debt signals.

## Cap policy: uniform now, fork-gated later

One conservative cap, applied to every peer, sized to stay under the reference peer's per-connection multiplexer budget (~32 streams shared across all protocols). A per-peer cap of ~4 leaves ample headroom for retrieval, pushsync, pricing, pseudosettle, swap, and identity on the same connection.

Capping is NOT keyed on peer type (reference vs vertex). The binding constraint is the per-connection multiplexer limit, which the reference peer enforces regardless of node type, so detecting the implementation buys nothing on the constraint that actually resets us. The signals that would carry node type (the identify agent string and the handshake node-type bool) are both unauthenticated and spoofable, and spoofing is asymmetric: a peer can only ever induce us to overrun, never underrun, so keying caps on type makes a safety guard depend on peer honesty. The payoff (a higher vertex-to-vertex cap) is marginal on a network where almost every retrieval counterpart is the reference node.

The cap is a single named constant with a clean seam: a later hardfork-gated, handshake-negotiated (cryptographically anchored, not identify-asserted) higher cap for vertex-to-vertex links can replace it once that case is worth optimising. Until then, type-derived behaviour is a non-goal.

## Two reset causes (keep distinct)

- Substream-budget overrun: too many concurrent substreams to one peer. The individual substream resets, the peer stays connected, later requests can succeed. This is what the per-peer cap fixes.
- Economic debt crossing: per-peer unsettled debt crosses the remote disconnect line. The whole connection drops, all in-flight substreams fail at once, the peer is removed. Tracked separately by the accounting work.

They are distinguishable in observability (stream-level reset with the peer still present vs a peer-removal event) and must be metered separately.

## nectar 0.3.0 adoption (consumer side)

nectar 0.3.0 adds wasm-ready out-of-order joiner primitives: a `ChunkGet` getter the joiner pulls through, a `WriteAt` sink, `into_offset_stream_chunked()` (out-of-order `(offset, bytes)`), and a one-call `download_into(sink)` with progress. The vertex bump from 0.2.x is non-breaking because vertex does not use nectar's file module today. These primitives are a consumer-side enabler for the browser demo download path; they are independent of the per-peer cap, which lives in vertex's own stream and retrieval layer.

## Sequencing

1. Document the unordered-delivery invariant and the concurrency taxonomy; retire "prefetch width". Doc-only, unblocks the structural change by making its core assumption a reviewed, greppable contract.
2. Per-peer in-flight cap with skip-busy spread in the candidate race, seeded from the single conservative constant. The structural fix.
3. Reconcile the wide last-resort retrieval race so it respects the per-peer caps (or runs under a tighter last-resort cap), so the wide path cannot recreate the storm.
4. Separate workstream: bump nectar to 0.3.0 and adopt `download_into`/`WriteAt` for the demo download path.

## Risks

- Thin peer set (browser): caps must be per-peer, never global; below a live-set threshold, fall back to the staggered race rather than refuse. Keep the browser pipeline depth small so the cap is rarely binding.
- All close peers busy: fall through to the staggered race; a failed slot acquisition advances the candidate walk rather than erroring.
- Thundering herd on slot release: avoided by construction; a non-blocking try-acquire never parks a herd of awaiters.
- Wide-race interaction: the last-resort race must consult the same cap or run under its own tighter cap.
- wasm cone: the limiter stays in the node layer (not the wasm-facing stream crate); verify it builds for `wasm32-unknown-unknown`.
- Width-as-correctness: enforced by the invariant plus a consumer-boundary test that delivers chunks in reverse order and asserts correct reassembly.
