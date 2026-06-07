## libp2p and networking guidance

This section codifies the libp2p discipline used in `vertex`. Treat `rust-libp2p` (`libp2p/rust-libp2p`) as the canonical model for `NetworkBehaviour`, `ConnectionHandler`, request-response and codec patterns. Use `multiaddrs` everywhere; do not use the word "underlay".

### 1. The libp2p boundary

Network crates may import `libp2p`. Domain crates may not.

May depend on `libp2p`:
- `crates/swarm/net/*` (`handshake`, `headers`, `hive`, `identify`, `pricing`, `pseudosettle`, `pingpong`, `pushsync`, `retrieval`, `swap`)
- `crates/swarm/node` (the boundary)
- `crates/swarm/topology`, `crates/swarm/peers/peer`, `crates/swarm/peers/peer-manager`
- `crates/net/dialer`, `crates/net/dnsaddr`, `crates/net/local`

Must NOT depend on `libp2p`:
- `crates/swarm/spec`, `crates/swarm/forks`, `crates/swarm/identity`, `crates/swarm/primitives`, `crates/swarm/storer`, `crates/swarm/localstore`, `crates/swarm/builder`, `crates/swarm/rpc`, `crates/swarm/redistribution`
- `crates/net/codec`, `crates/net/ratelimiter`

`vertex-swarm-api` currently re-exports `libp2p::Multiaddr` and uses it in a small number of config and topology trait signatures (`crates/swarm/api/src/lib.rs:91`, `crates/swarm/api/src/components/topology.rs`). This is a deliberate exception, not a license to import `libp2p` more broadly into the API; new traits should prefer `OverlayAddress` and `SwarmPeer` (from `vertex-swarm-peer`, the designated `Multiaddr` boundary crate). See `docs/client/architecture.md`.

### 2. NetworkBehaviour rules

One protocol, one `NetworkBehaviour`, one `ConnectionHandler`. Do not multiplex protocols inside a single behaviour. Composition happens exclusively in `vertex-swarm-node` via `#[derive(NetworkBehaviour)]` (see `crates/swarm/node/src/node/client.rs:34`, which composes `identify`, `topology`, `client`). Bootnode and storer node variants live as separate composed behaviours under `crates/swarm/node/src/node/`.

Model new protocols on `libp2p::request_response` for one-shot exchanges and on the existing `handshake`/`pingpong`/`retrieval` crates for streamed flows. Every protocol crate exposes a `Behaviour` and a `Handler`; emit `ToBehaviour` events, never side-channel through globals.

### 3. Streams, codecs, framing

- Protobuf is `quick-protobuf` plus `quick-protobuf-codec` (`crates/swarm/net/{handshake,headers,identify,pricing,pushsync,retrieval,swap,hive}/Cargo.toml`). Generated `structs.rs` uses `quick_protobuf::{BytesReader, MessageRead, MessageWrite, Writer}`.
- Length-prefixed framing goes through `vertex-net-codec`: use `FramedProto<BUF>` (`crates/net/codec/src/framed.rs:39`) over `asynchronous_codec::Framed`. The `BUF` const fixes the per-protocol max message size.
- Per-protocol `Codec` impls live next to the protocol: in the protocol crate (e.g. `crates/swarm/net/handshake/src/codec/`) or in `vertex-net-codec` when truly generic. Do not invent a new codec where `quick_protobuf_codec::Codec::<Msg>::new(MAX)` suffices (see `crates/swarm/net/identify/src/protocol.rs:85`).
- All protocols except `handshake` are headered streams: the headers exchange runs first, then protocol data. Build new headered protocols on `vertex-swarm-net-headers`' `InnerInbound`, `InnerOutbound`, `HeaderedInbound<T>`, `HeaderedOutbound<T>` (`crates/swarm/net/headers/src/lib.rs`). See `docs/swarm/protocols.md`.

### 4. PeerId vs OverlayAddress

`PeerId` is libp2p transport identity. `OverlayAddress` is the Swarm Kademlia key, derived as `keccak256(ethereum_address || network_id_le8 || nonce)` via `nectar_primitives::compute_overlay`. It is not the Ethereum key (or address) alone; the same operator identity yields different overlays across networks or with a new nonce, which is the property the redistribution game relies on. The `PeerId`/`OverlayAddress` mapping is owned by `vertex-swarm-node` (with the `PeerRegistry` in `vertex-net-peer-registry`). Never expose `PeerId` above `vertex-swarm-node`: traits in `vertex-swarm-api`, accounting, storer, RPC, and CLI all speak `OverlayAddress`. See `docs/client/architecture.md` for the dependency graph and `crates/swarm/peers/peer-manager/src/manager.rs` for the `DashMap<OverlayAddress, Arc<PeerEntry>>` model.

### 5. Connection lifecycle and backoff

Peer state is Arc-per-peer via a registry. Obtain `Arc<PeerEntry>` once; afterwards everything is lock-free atomics or per-peer locks. Backoff is its own crate (`vertex-net-peer-backoff`) and must not be reimplemented inside protocol handlers. Connection states are `Known`, `Connecting`, `Connected`, `Disconnected`, `Banned`. Score uses fixed-point atomics (`vertex-net-peer-score`). Eviction is driven by the topology's `SaturationDecision` and the peer-manager's ban set (`banned_set: DashSet<OverlayAddress>`), not by protocol handlers acting unilaterally. See `docs/networking/peer-management.md`.

### 6. Dialer discipline

`vertex-net-dialer` owns candidate selection, tracking, and exponential backoff with jitter (`crates/net/dialer/src/{backoff,tracker,prepare}.rs`). Bootstrap is a three-phase flow: parallel bootnode dials, hive discovery, Kademlia bin filling. mDNS is not used in production; `dnsaddr` is for bootnode and operator discovery only and is resolved recursively (`vertex-net-dnsaddr` follows all TXT records, unlike libp2p's DNS transport). DHT-style discovery is performed via the hive protocol, not Kademlia content routing. See `docs/networking/peer-dialing-strategy.md`.

### 7. Address management

`vertex-net-local` owns IP scope classification (`loopback`, `private`, `link-local`, `public`), dual-stack capability, and same-subnet detection via `netdev` (cached 60s). `vertex-swarm-topology`'s `LocalAddressManager` (in `nat_discovery`) selects addresses for handshake advertisement, sourcing from `--nat-addr` (highest priority) and listen addresses. Addresses are always `libp2p::Multiaddr` with a trailing `/p2p/{local_peer_id}`. Observed addresses from identify are used only to flip a public-connectivity flag; do not store or advertise NAT-mapped ephemeral ports. See `docs/networking/address-management.md`.

### 8. Handshake and headers gotchas

Handshake is the only non-headered Swarm protocol; it speaks SYN/ACK directly. Every other protocol must complete the headers exchange before any protocol payload moves. When writing a new protocol: wrap the inner codec in `HeaderedInbound<T>` / `HeaderedOutbound<T>`. Handler output arrives as `HeaderedInboundOutput<T>` with a `.data` field. The custom `0x99` multi-multiaddr encoding lives in `vertex-swarm-peer` and is not standard libp2p; do not propagate it into other protocols.

### 9. Rate limiting and admission control

`vertex-net-ratelimiter` provides GCRA buckets (`RateLimiter`, `KeyedRateLimiter`); it is libp2p-agnostic and sits below the protocol crates. Wire it into per-protocol handlers at the codec boundary, not into the swarm event loop.

Admission control for inbound connections is a trait, not a policy hardcoded in handshake. Implement `HandshakeAdmissionControl` (`crates/swarm/net/handshake/src/admission.rs`) and pass it as `SharedAdmissionControl = Arc<dyn HandshakeAdmissionControl>` via `HandshakeBehaviour::with_admission_control`. `default_admission_control()` returns always-accept. Higher layers (Kademlia saturation, peer scoring, ban list) compose into this trait; do not bolt new gating logic onto the handshake handler.

### 10. Protocol conformance tests

Wire-conformance vectors for the handshake live at `crates/swarm/peers/peer/tests/interop.rs`. They pin two layers: overlay derivation (`keccak256(eth_address || network_id_le(8) || nonce(32))`) and handshake 15.0.0 sign-data with deterministic RFC 6979 secp256k1 ECDSA. Vectors are struct literals (`OverlayVector`, handshake vectors) with hex inputs and expected hex outputs.

To add a new conformance vector:
1. Drive the public API of the relevant crate (`SwarmPeer::sign`, `SwarmPeer::parse`), not a re-implementation of the byte layout.
2. Add the literal triple (inputs, expected bytes) to the matching `&[Vector]` table in `tests/interop.rs` (or a new `tests/<protocol>.rs` for a new protocol).
3. If the values come from another conforming implementation, cite the source in a doc comment; if they are first-defined here, document the byte layout in the test header so future implementations can match.
4. Run under `cargo test -p vertex-swarm-peer --test interop`; mismatches indicate one-byte drift in either the sign-data or the overlay derivation.

New protocols ship with their own conformance test file under the protocol crate's `tests/`. Treat these as the wire contract.
