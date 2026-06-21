# AGENTS: crates/swarm/net/

The `/swarm/...` wire protocols. Each crate here defines exactly one libp2p protocol: behaviour, connection handler, codec, error, metrics. `proto` holds every generated protobuf module so individual protocols depend only on `vertex-swarm-net-proto`, never on a build script of their own.

Root-level rules in `/AGENTS.md` apply here too. The notes below are the area-specific overlay. The "Swarm protocol guidance" and "libp2p and networking guidance" sections of the root file are the primary reading for changes here.

## Crates and protocol IDs

- `handshake`: `/swarm/handshake/15.0.0/handshake`. Identity exchange and admission control. Non-headered.
- `hive`: signed peer-record gossip for topology bootstrap.
- `pricing`: `/swarm/pricing/1.0.0/pricing`. Payment threshold announcement.
- `pseudosettle`: `/swarm/pseudosettle/1.0.0/pseudosettle`. Bandwidth micro-payments.
- `pullsync`: `/swarm/pullsync/1.4.0/cursors` (Syn/Ack cursor handshake) and `/swarm/pullsync/1.4.0/pullsync` (Get/Offer/Want/Delivery range exchange). One protocol, two streams: the "one `PROTOCOL_NAME` per crate" rule bends here, exposed as `PROTOCOL_CURSORS` and `PROTOCOL_SYNC`.
- `pushsync`: `/swarm/pushsync/1.3.1/pushsync`. Chunk push with receipts.
- `retrieval`: `/swarm/retrieval/1.4.0/retrieval`. Chunk request and delivery.
- `headers`: shared header frame used by request-response protocols, with trace-context propagation. W3C propagation over OpenTelemetry is native-only (`tracing.rs`); the wasm sibling (`tracing_wasm.rs`, Pattern C) is no-op inject/extract since a browser client has no OTLP backend. The on-wire `tracing-span-context` field is unaffected.
- `handler-core`: shared `HandlerCore<E>` for handlers (pending events, GCRA, outbound-pending flag).
- `identify`: vendored libp2p-identify with targeted-push extension.
- `proto`: consolidated protobuf modules. Re-exports `handshake`, `hive`, `pricing`, `pseudosettle`, `pushsync`, `retrieval`, `headers`, `pullsync`, `swap`.

## Dos

- One protocol per crate, one `PROTOCOL_NAME` constant per crate. Reference that constant from tests and metrics labels.
- Implement the codec in its own `codec` module, separate from the behaviour. Wire types live behind a domain wrapper so the protobuf type never escapes.
- Compose `vertex-net-codec::FramedProto` for framing. Use the `protocol_error!` macro from `vertex-net-codec` to generate the common error variants (`ConnectionClosed`, `Protobuf`, `Io`).
- Compose `HandlerCore` for the rate-limited inbound queue and outbound flag.
- Embed metrics in a `pub mod metrics` submodule and derive `strum::IntoStaticStr` on event/stage enums so labels are static strs.
- Pull the protobuf module from `vertex-swarm-net-proto`. Never add a new `build.rs` or `OUT_DIR` include path in a protocol crate.

## Donts

- Do not inline the protobuf module into the behaviour file. Generated code lives in `vertex-swarm-net-proto` and is consumed via `pub use`.
- Do not let the protobuf type escape the codec boundary. The behaviour deals in domain types.
- Do not depend on `vertex-swarm-storer`, `vertex-swarm-localstore`, or `vertex-swarm-topology` from a protocol crate. Protocol crates expose traits and let higher layers wire them.
- Do not assume a single connection per peer in handler state. The `KeyedRateLimiter` exists so behaviour-level limits work across handlers.
- Do not use `unwrap`/`expect` on protobuf decoding paths. The workspace lints them as warnings on purpose.

## Wire conformance

Any wire-visible change must be paired with a conformance vector update under the protocol crate's `tests/`. See `crates/swarm/peers/peer/tests/interop.rs` as the model. Wire changes that do not match the reference implementation must be gated behind a `SwarmHardfork` and selected via `ForkDigest`; never feature-flag wire bytes.

## Tests and interop

- `cargo test -p vertex-swarm-net-<name>` for unit tests.
- `handshake` carries cross-implementation wire-conformance vectors. Run `cargo test -p vertex-swarm-net-handshake` after any wire change and update the vector files if the change is intentional.
- `libp2p-swarm-test` is the harness for behaviour-level tests.
