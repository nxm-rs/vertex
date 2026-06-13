# Threat model: outbound self-throttle (#131, #132)

## Design (5-10 lines)

Outbound pushsync and retrieval consume the pseudosettle credit a remote peer extends us; issuing faster than the peer forgives our debt wastes round trips on refusals and, sustained, trips the peer's refuse-or-disconnect threshold and drops us.
`SelfThrottle` (crates/swarm/node/src/throttle.rs) paces our own send rate under that allowance. It is the single seam both protocols share: one `SelfRateLimiter<OverlayAddress>` keyed by the peer's overlay, fed by one per-peer allowance signal (`PeerAffordability::allowance_remaining`, built once in the accounting layer), so the two protocols cannot pace against divergent views of the same allowance.
Tokens are settle units: one token is `settle_unit_size` AU, where `settle_unit_size` is the pseudosettle per-second forgiveness rate (`refresh_rate` AU/s). The bucket therefore refills at one token per second (matching debt forgiveness) and a peer's capacity is its remaining allowance in settle units. Cost per request is `ceil(chunk_cost / settle_unit_size)`: a fixed pushsync per-chunk cost and a flat retrieval estimate (the max-chunk cost, refined later per #132).
The throttle is consulted at the outbound API (`ClientHandle::retrieve_chunk` / `push_chunk`) before a command is dispatched. `acquire` re-syncs the live allowance into the peer's bucket (`SelfRateLimiter::set_quota`, which re-sizes the GCRA cell for that key only), then either returns at once or sleeps the bucket's wait hint and retries; the first delay increments `pushsync_self_throttled_total{peer_overlay}` or `retrieval_self_throttled_total{peer_overlay}`. On disconnect the client service clears the peer's bucket.

## Trust boundary

The remote peer is untrusted. We never read the remote's allowance from the wire: the allowance signal is OUR accounting layer's own `allowance_remaining` (balance plus disconnect threshold less reservations), computed locally from state we control. The throttle is a self-imposed politeness/efficiency control on OUR outbound rate, not an authorization check, so it cannot be turned into a remote-controlled denial of our own service. Inbound rate limiting (a remote driving requests at us) is handled separately by the accounting layer; this unit is outbound only.

## Attacks, abuse, and failure modes considered

- Allowance shrinks to zero or a peer extends us nothing. `sync_quota` floors the capacity at one token and `settle_unit_size`/`max_chunk_cost` are floored at one AU in the constructor, so the GCRA quota math never divides by zero or builds a zero-`NonZeroU32`; such a peer is throttled hard but the path stays panic-free (test `zero_allowance_yields_a_valid_one_token_bucket`).
- Allowance collapses mid-flight (adversarial or churny accounting). `set_key_quota` re-clamps a drained bucket's outstanding TAT into the smaller capacity, so a shrink throttles immediately instead of letting stale oversized credit through (tests `set_key_quota_shrink_clamps_outstanding_credit`, `set_quota_shrink_throttles_immediately`, `allowance_shrink_throttles_immediately`).
- Free-burst replay: re-applying an unchanged allowance must not reset the bucket and hand a fresh burst. `set_key_quota` is idempotent on an unchanged quota (compares the derived cell), so repeated syncs do not reset the TAT (test `set_key_quota_is_idempotent_no_free_burst`).
- Persistently collapsing allowance could park a request forever. `acquire` caps retries at `MAX_THROTTLE_ITERATIONS` and then releases the request rather than hanging the future; a refused request is recoverable, a stuck future is not.
- Oversized cost (a single request that can never fit the bucket). `try_send` maps `TooLarge` to the per-key replenish window rather than the limiter-wide default, so the wait hint is correct for that peer (test `try_send_too_large_reports_per_key_window`); the constructor's AU floors keep the cost from being unbounded.
- Cross-peer interference. Buckets are per-overlay and per-key quotas are independent, so one peer's allowance or drain cannot throttle another (tests `per_peer_buckets_are_independent`, `set_key_quota_resizes_only_that_key`).
- Teardown / memory growth. On `PeerDisconnected` the client service calls `SelfThrottle::clear`, dropping the peer's bucket so memory does not grow with the count of distinct peers seen and a reconnect starts from a fresh allowance rather than stale credit (test `clear_drops_peer_bucket`). The handle and the service share the same `Arc<SelfThrottle>`, so the cleared bucket is the one the outbound API paces against.
- Metric cardinality. `peer_overlay` is a per-peer label as the issues require; the counter increments at most once per `acquire` call (only on the first delay), bounding write rate. Operators aggregating over the label should be aware of the per-peer cardinality.
- Backward compatibility / no wire change. The throttle is purely local outbound pacing: no bytes change on the wire, so no `SwarmHardfork` gate is needed. A handle built without a throttle dispatches exactly as before (test `unthrottled_handle_dispatches_immediately`).
- Wasm. The parking timer is `futures-timer` (the workspace's wasm-safe timer) and the limiter uses `vertex-util-runtime`'s `web-time` clock, so the whole path builds and runs in the `vertex-swarm-node` wasm cone (CI-enforced).

## Deferred gaps

- Retrieval cost is a flat max-chunk estimate because the response size is unknown before it arrives. Refining it once response-size measurements exist is tracked in #132 (TODO in the cost-model rustdoc).
- The parking timer does not honor tokio's paused test clock; throttle timing tests run on the real clock with short windows. This is a test-ergonomics limitation, not a production gap.
